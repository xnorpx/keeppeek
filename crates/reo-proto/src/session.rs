//! Core SANS-IO Baichuan session state machine.
//!
//! `BcSession` never performs I/O, spawns threads, or calls `Instant::now()`.
//! All time information comes from caller-provided `Instant` values.
//! The caller drives the session with `handle_input()` / `poll_output()`.

use crate::{
    NONCE_CAP,
    auth::{self, EncryptionMode, LoginParams, LoginResult},
    error::BcError,
    header::PacketHeader,
    magic::*,
};
use arrayvec::{ArrayString, ArrayVec};
use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

const MAX_PENDING: usize = 128;
const MAX_PENDING_COMMANDS: usize = 128;
const MAX_MISSED_PINGS: u8 = 5;

/// Media frames are padded to this alignment on the wire.
const MEDIA_FRAME_ALIGNMENT: usize = 8;

/// Upper bound on a media frame header, used to bound the split-header carry buffer.
const MAX_MEDIA_HEADER: usize = 512;

/// Session role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Client,
    Camera,
}

/// Session state (visible for testing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    // Client states
    Disconnected,
    AwaitingNonce,
    AwaitingLoginConfirm,
    Connected,
    // Camera states
    AwaitingLogin,
    AwaitingModernLogin,
    Authenticated,
}

/// Session configuration. All buffer sizes are in bytes.
#[derive(Debug, Clone)]
pub struct BcSessionConfig {
    pub role: Role,
    pub tcp_recv_buf_size: usize,
    pub tcp_send_buf_size: usize,
    pub keepalive_channel: u8,
    pub keepalive_interval: Duration,
    pub stream_watchdog_interval: Duration,
    pub relogin_interval: Duration,
}

impl BcSessionConfig {
    pub const fn default_client() -> Self {
        Self {
            role: Role::Client,
            tcp_recv_buf_size: crate::TCP_RECV_BUF_SIZE,
            tcp_send_buf_size: crate::TCP_SEND_BUF_SIZE,
            keepalive_channel: 0,
            keepalive_interval: Duration::from_secs(10),
            stream_watchdog_interval: Duration::from_secs(30),
            relogin_interval: Duration::from_secs(300),
        }
    }

    pub const fn default_camera() -> Self {
        Self {
            role: Role::Camera,
            tcp_recv_buf_size: crate::TCP_RECV_BUF_SIZE,
            tcp_send_buf_size: crate::TCP_SEND_BUF_SIZE,
            keepalive_channel: 0,
            keepalive_interval: Duration::from_secs(10),
            stream_watchdog_interval: Duration::from_secs(30),
            relogin_interval: Duration::from_secs(300),
        }
    }
}

/// Input to the session state machine.
#[allow(clippy::large_enum_variant)]
pub enum Input<'a> {
    /// Time has advanced. Drives keepalive and watchdog timers.
    Timeout(Instant),
    /// Raw bytes arrived from the TCP socket.
    TcpData(Instant, &'a [u8]),
    /// Application issues a command.
    Command(Command),
}

/// Output from the session state machine.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Output<'buf> {
    /// Send these bytes over the TCP connection.
    TcpSend { data: &'buf [u8] },
    /// An event occurred.
    Event(Event<'buf>),
    /// Nothing more pending. Wake the session at this deadline.
    Timeout(Instant),
}

/// Commands that can be issued to the session.
#[derive(Debug)]
pub enum Command {
    /// Send a keepalive ping.
    Ping,
    /// Start the login handshake.
    Login(LoginParams),
    /// Send a logout and disconnect.
    Logout,
    /// Start a video stream.
    StartStream(crate::stream::StreamRequest),
    /// Subscribe to a stream and receive a local stream id.
    SubscribeStream(crate::stream::StreamSubscription),
    /// Unsubscribe (stop) a stream by local stream id.
    UnsubscribeStream { stream_id: u32 },
    /// Stop a video stream.
    StopStream(crate::stream::StreamStop),
    /// Request a JPEG snapshot.
    Snapshot(crate::stream::SnapshotRequest),
    /// Open an external stream for talkback control and audio packets.
    OpenTalkback { channel: u8 },
    /// Reset talkback and close its external stream.
    CloseTalkback { channel: u8 },
    /// Send a talkback protocol command over an external stream.
    Talk(crate::talk::TalkCommand),
    /// Device / system query or config command.
    Device(crate::device::DeviceCommand),
    /// Video / encoding query or config command.
    Video(crate::video_cfg::VideoCommand),
    /// Network query or config command.
    Network(crate::network_cfg::NetworkCommand),
    /// PTZ command.
    Ptz(crate::ptz::PtzCommand),
    /// Alarm / detection command.
    Alarm(crate::alarm::AlarmCommand),
    /// Recording / storage command.
    Recording(crate::recording::RecordingCommand),
    /// Notification / output device command.
    Notification(crate::notification::NotificationCommand),
}

/// Events emitted by the session.
#[derive(Debug)]
pub enum Event<'buf> {
    /// Keepalive timer expired with no activity (stream watchdog).
    SessionTimeout,
    /// Received a ping response (keepalive acknowledged).
    Pong,
    /// A tracked command completed successfully.
    CommandCompleted {
        msg_id: u32,
        msg_num: u16,
        status: u16,
    },
    /// A tracked command was rejected by the camera.
    CommandFailed {
        msg_id: u32,
        msg_num: u16,
        status: u16,
    },
    /// Received a message with no specific handler.
    UnhandledMessage { msg_id: u32, body: &'buf [u8] },
    /// Login handshake completed successfully.
    LoggedIn(LoginResult),
    /// Login handshake failed (status from camera header).
    LoginFailed(u32),
    /// Stream info header received (resolution, fps, timestamps).
    StreamMetadata {
        stream_id: u32,
        info: crate::media::StreamMetadata,
    },
    /// A video frame was received. Data is NAL-normalized Annex B.
    VideoFrame {
        stream_id: u32,
        channel: u8,
        is_keyframe: bool,
        codec: crate::media::VideoCodec,
        data: &'buf [u8],
        microseconds: u32,
    },
    /// An audio frame was received.
    AudioFrame {
        stream_id: u32,
        codec: crate::media::AudioCodec,
        data: &'buf [u8],
    },
    /// JPEG snapshot data received.
    SnapshotData { data: &'buf [u8] },
    /// Snapshot transfer was rejected by the camera.
    SnapshotFailed { status: u16 },
    /// Talkback protocol event.
    Talk(crate::talk::TalkEvent),
    /// Stream started acknowledgement.
    StreamStarted,
    /// Stream was subscribed and assigned a local stream id.
    StreamSubscribed {
        stream_id: u32,
        channel: u8,
        stream_type: crate::stream::StreamType,
    },
    /// Stream was unsubscribed by local stream id.
    StreamUnsubscribed { stream_id: u32 },
    /// Stream stopped acknowledgement.
    StreamStopped,
    /// Device / system event from domain module.
    Device(crate::device::DeviceEvent),
    /// Video / encoding event from domain module.
    Video(crate::video_cfg::VideoEvent),
    /// Network event from domain module.
    Network(crate::network_cfg::NetworkEvent),
    /// PTZ event from domain module.
    Ptz(crate::ptz::PtzEvent),
    /// Alarm / detection event from domain module.
    Alarm(crate::alarm::AlertEvent),
    /// Recording / storage event from domain module.
    Recording(crate::recording::RecordingEvent),
    /// Notification / output device event from domain module.
    Notification(crate::notification::NotificationEvent),
    /// Binary file data from recording playback.
    FileData { data: &'buf [u8] },
    /// Binary thumbnail data from recording thumbnail request.
    ThumbnailData { data: &'buf [u8] },
}

#[derive(Debug, Clone, Copy)]
struct PendingEvent {
    kind: EventKind,
    body_start: usize,
    body_len: usize,
    /// When true, body_start/body_len reference `media_out` instead of `recv_buf`.
    from_media: bool,
}

/// In-progress video frame being assembled from multiple BC messages.
struct VideoAccum {
    stream_id: u32,
    channel: u8,
    is_keyframe: bool,
    codec: crate::media::VideoCodec,
    microseconds: u32,
    expected_data_len: usize,
    data: Vec<u8>,
}

/// In-progress audio frame being assembled from multiple BC messages.
struct AudioAccum {
    stream_id: u32,
    codec: crate::media::AudioCodec,
    expected_data_len: usize,
    padding: usize,
    data: Vec<u8>,
}

struct SnapshotAccum {
    expected_data_len: usize,
    data: Vec<u8>,
}

const fn response_status_is_success(status: u16) -> bool {
    matches!(status, 0 | 200 | 201 | 300)
}

fn parse_snapshot_size(metadata: &[u8]) -> Result<usize, BcError> {
    let size = crate::xml::extract_u32(metadata, "pictureSize")?
        .ok_or(BcError::Protocol("snapshot metadata missing pictureSize"))? as usize;
    if size == 0 || size > crate::MAX_SNAPSHOT_BYTES {
        return Err(BcError::Protocol(
            "snapshot size is outside accepted bounds",
        ));
    }
    Ok(size)
}

fn snapshot_extension_is_binary(extension: &[u8]) -> Result<bool, BcError> {
    if extension.is_empty() {
        return Ok(false);
    }
    let mut binary = false;
    crate::xml::parse_xml(extension, |name, text| {
        if matches!(name, "binaryData" | "binary") {
            binary = text.trim() == "1" || text.trim().eq_ignore_ascii_case("true");
        }
    })?;
    Ok(binary)
}

fn payload_range(
    header: PacketHeader,
    body_start: usize,
    body_len: usize,
) -> Result<(usize, usize), BcError> {
    let payload_offset = header.extension.unwrap_or(0) as usize;
    if payload_offset > body_len {
        return Err(BcError::InvalidHeader(
            "payload offset exceeds message body length",
        ));
    }
    Ok((body_start + payload_offset, body_len - payload_offset))
}

#[derive(Debug, Clone, Copy)]
struct StreamSubscriptionEntry {
    channel: u8,
    stream_type: crate::stream::StreamType,
    expected_width: u32,
    expected_height: u32,
}

impl PendingEvent {
    /// Create a PendingEvent referencing data in recv_buf.
    const fn new(kind: EventKind, body_start: usize, body_len: usize) -> Self {
        Self {
            kind,
            body_start,
            body_len,
            from_media: false,
        }
    }

    /// Create a PendingEvent referencing data in media_out.
    const fn media(kind: EventKind, body_start: usize, body_len: usize) -> Self {
        Self {
            kind,
            body_start,
            body_len,
            from_media: true,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum EventKind {
    SessionTimeout,
    Pong,
    CommandCompleted {
        msg_id: u32,
        msg_num: u16,
        status: u16,
    },
    CommandFailed {
        msg_id: u32,
        msg_num: u16,
        status: u16,
    },
    Unhandled {
        msg_id: u32,
    },
    LoggedIn,
    LoginFailed {
        status: u32,
    },
    /// Stream info header (resolution, fps, etc).
    MediaInfo {
        stream_id: u32,
        info: crate::media::StreamMetadata,
    },
    /// Video frame -- body_start/len point to raw video data in recv_buf.
    VideoFrame {
        stream_id: u32,
        channel: u8,
        is_keyframe: bool,
        codec: crate::media::VideoCodec,
        microseconds: u32,
    },
    /// Audio frame -- body_start/len point to raw audio data in recv_buf.
    AudioFrame {
        stream_id: u32,
        codec: crate::media::AudioCodec,
    },
    /// JPEG snapshot data.
    SnapshotData,
    SnapshotFailed {
        status: u16,
    },
    /// Talkback domain response (lazy-parsed at poll_output time).
    TalkResponse(crate::talk::TalkResponseKind),
    /// Stream started ack (response to start request).
    StreamStarted,
    /// Stream subscribed and assigned a local id.
    StreamSubscribed {
        stream_id: u32,
        channel: u8,
        stream_type: crate::stream::StreamType,
    },
    /// Stream unsubscribed by local stream id.
    StreamUnsubscribed {
        stream_id: u32,
    },
    /// Stream stopped ack (response to stop request).
    StreamStopped,
    /// Device domain response (lazy-parsed at poll_output time).
    DeviceResponse(crate::device::DeviceResponseKind),
    /// Video config domain response (lazy-parsed at poll_output time).
    VideoResponse(crate::video_cfg::VideoResponseKind),
    /// Network domain response (lazy-parsed at poll_output time).
    NetworkResponse(crate::network_cfg::NetworkResponseKind),
    /// PTZ domain response (lazy-parsed at poll_output time).
    PtzResponse(crate::ptz::PtzResponseKind),
    /// Alarm domain response (lazy-parsed at poll_output time).
    AlarmResponse(crate::alarm::AlarmResponseKind),
    /// Recording domain response (lazy-parsed at poll_output time).
    RecordingResponse(crate::recording::RecordingResponseKind),
    /// Notification domain response (lazy-parsed at poll_output time).
    NotificationResponse(crate::notification::NotificationResponseKind),
    /// Binary file data from recording playback.
    FileData,
    /// Binary thumbnail data.
    ThumbnailData,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingCommandKind {
    Generic,
    Ping,
    Snapshot,
}

/// Diagnostic counters for tracking frame delivery and losses.
#[derive(Debug, Clone, Default)]
pub struct SessionStats {
    /// Total COMMAND_STREAM messages received.
    pub stream_messages: u64,
    /// Video frames successfully emitted (single-message + accumulated).
    pub video_frames_emitted: u64,
    /// Audio frames successfully emitted.
    pub audio_frames_emitted: u64,
    /// Times a chunked video accumulation was started.
    pub video_accum_started: u64,
    /// Times a chunked video accumulation completed successfully.
    pub video_accum_completed: u64,
    /// Times a chunked video accumulation was abandoned by a new frame.
    pub video_accum_abandoned: u64,
    /// Times a chunked audio accumulation was started.
    pub audio_accum_started: u64,
    /// Times a chunked audio accumulation completed successfully.
    pub audio_accum_completed: u64,
    /// Frame headers that straddled a message boundary and were carried over.
    pub split_headers: u64,
    /// Continuation chunks appended to the video accumulator.
    pub continuation_chunks: u64,
    /// Frames/events dropped because the pending queue was full.
    pub pending_drops: u64,
    /// Bytes skipped by the BC header resynchronisation scan.
    pub resync_skipped_bytes: u64,
    /// COMMAND_STREAM bodies that matched no media frame, accumulator, or XML.
    pub stream_bodies_unrecognized: u64,
    /// Trailing bytes after a media frame that did not start a known frame.
    pub trailing_bytes_unrecognized: u64,
    /// Continuation chunks that could not be attributed to one accumulator.
    pub continuation_ambiguous: u64,
    /// Media bodies that began with the previous frame's alignment padding.
    pub padded_media_bodies: u64,
}

/// The core SANS-IO Baichuan protocol session.
///
/// Preallocates all internal buffers on construction. At steady state,
/// no heap allocations occur.
pub struct BcSession {
    role: Role,
    state: SessionState,

    // TCP receive buffer (preallocated)
    recv_buf: Box<[u8]>,
    recv_start: usize, // start of unprocessed data
    recv_end: usize,   // end of written data

    // Send staging buffer (preallocated)
    send_buf: Box<[u8]>,
    send_start: usize, // start of unsent data
    send_end: usize,   // end of staged data

    // Pending events (body data stays in recv_buf)
    pending: ArrayVec<PendingEvent, MAX_PENDING>,
    pending_idx: usize,

    // Auth state
    login_params: Option<LoginParams>,
    nonce: ArrayString<NONCE_CAP>,
    encryption: EncryptionMode,
    aes_key: Option<[u8; 16]>,
    login_result: Option<LoginResult>,

    // Timers
    last_recv: Instant,
    last_send: Instant,
    last_login: Option<Instant>,
    keepalive_interval: Duration,
    keepalive_channel: u8,
    stream_watchdog_interval: Duration,
    relogin_interval: Duration,
    active_streams: u8,
    next_stream_id: u32,
    next_msg_num: u16,
    pending_commands: HashMap<(u32, u16), PendingCommandKind>,
    pending_ping: Option<u16>,
    missed_pings: u8,
    stream_ids_by_key: HashMap<u16, u32>,
    stream_subs_by_id: HashMap<u32, StreamSubscriptionEntry>,
    pending_subscribe_ids: VecDeque<u32>,
    stream_id_by_remote_handle: HashMap<u32, u32>,
    remote_handle_by_stream_id: HashMap<u32, u32>,
    /// msg_num → stream_id mapping. Each outgoing stream request gets a
    /// unique msg_num packed into encryption_offset bytes 14-15.  If the
    /// camera echoes it back, we can route by msg_num.
    stream_id_by_msg_num: HashMap<u16, u32>,
    /// (channel, codec) → stream_id mapping. Used as fallback when the camera
    /// doesn't differentiate streams in the BC header or video header hint.
    /// Keyed on (media_channel, codec) so multi-channel cameras (e.g. Duo 3)
    /// with the same codec on different channels are distinguished.
    stream_id_by_channel_codec: HashMap<(u8, crate::media::VideoCodec), u32>,

    /// Last parsed stream metadata (resolution/fps). Set when InfoV1/V2 frames
    /// arrive. Used to match auto-learned stream IDs by resolution.
    last_stream_metadata: Option<crate::media::StreamMetadata>,

    // Media frame accumulation for chunked video data from camera
    video_accums: HashMap<u32, VideoAccum>,
    audio_accums: HashMap<u32, AudioAccum>,
    header_carry: Vec<u8>,
    /// Staging buffer for completed media and snapshot data (referenced by
    /// PendingEvents that have from_media=true). Cleared when all pending events are drained.
    media_out: Vec<u8>,
    snapshot_accum: Option<SnapshotAccum>,
    snapshot_in_flight: bool,

    // Diagnostic counters
    stats: SessionStats,
}

impl BcSession {
    /// Create a new session with the given configuration.
    pub fn new(config: BcSessionConfig, now: Instant) -> Self {
        let initial_state = match config.role {
            Role::Client => SessionState::Disconnected,
            Role::Camera => SessionState::AwaitingLogin,
        };
        Self {
            role: config.role,
            state: initial_state,
            recv_buf: vec![0u8; config.tcp_recv_buf_size].into_boxed_slice(),
            recv_start: 0,
            recv_end: 0,
            send_buf: vec![0u8; config.tcp_send_buf_size].into_boxed_slice(),
            send_start: 0,
            send_end: 0,
            pending: ArrayVec::new(),
            pending_idx: 0,
            login_params: None,
            nonce: ArrayString::new(),
            encryption: EncryptionMode::None,
            aes_key: None,
            login_result: None,
            last_recv: now,
            last_send: now,
            last_login: None,
            keepalive_interval: config.keepalive_interval,
            keepalive_channel: config.keepalive_channel,
            stream_watchdog_interval: config.stream_watchdog_interval,
            relogin_interval: config.relogin_interval,
            active_streams: 0,
            next_stream_id: 1,
            next_msg_num: 1,
            pending_commands: HashMap::new(),
            pending_ping: None,
            missed_pings: 0,
            stream_ids_by_key: HashMap::new(),
            stream_subs_by_id: HashMap::new(),
            pending_subscribe_ids: VecDeque::new(),
            stream_id_by_remote_handle: HashMap::new(),
            remote_handle_by_stream_id: HashMap::new(),
            stream_id_by_msg_num: HashMap::new(),
            stream_id_by_channel_codec: HashMap::new(),
            last_stream_metadata: None,
            video_accums: HashMap::new(),
            audio_accums: HashMap::new(),
            header_carry: Vec::new(),
            media_out: Vec::new(),
            snapshot_accum: None,
            snapshot_in_flight: false,
            stats: SessionStats::default(),
        }
    }

    /// Create a client session with default configuration.
    pub fn default_client(now: Instant) -> Self {
        Self::new(BcSessionConfig::default_client(), now)
    }

    /// Create a camera session with default configuration.
    pub fn default_camera(now: Instant) -> Self {
        Self::new(BcSessionConfig::default_camera(), now)
    }

    /// Which role this session plays.
    pub const fn role(&self) -> Role {
        self.role
    }

    /// Current session state.
    pub const fn state(&self) -> SessionState {
        self.state
    }

    /// Number of active streams.
    pub const fn active_streams(&self) -> u8 {
        self.active_streams
    }

    /// Diagnostic counters for frame delivery tracking.
    pub const fn stats(&self) -> &SessionStats {
        &self.stats
    }

    /// Feed input into the state machine.
    pub fn handle_input(&mut self, input: Input) -> Result<(), BcError> {
        match input {
            Input::Timeout(now) => self.handle_timeout(now),
            Input::TcpData(now, data) => self.handle_tcp_data(now, data),
            Input::Command(cmd) => self.handle_command(cmd),
        }
    }

    /// Poll the next output. The caller provides a buffer for payload data.
    ///
    /// Returns `Output::Timeout` when nothing more is pending.
    /// All payload slices in the returned `Output<'buf>` borrow from `buf`.
    pub fn poll_output<'buf>(&mut self, buf: &'buf mut [u8]) -> Result<Output<'buf>, BcError> {
        // 1. Drain queued sends first
        if self.send_start < self.send_end {
            let available = self.send_end - self.send_start;
            let n = available.min(buf.len());
            buf[..n].copy_from_slice(&self.send_buf[self.send_start..self.send_start + n]);
            self.send_start += n;
            if self.send_start == self.send_end {
                self.send_start = 0;
                self.send_end = 0;
            }
            return Ok(Output::TcpSend { data: &buf[..n] });
        }

        // 2. Drain pending events
        if self.pending_idx < self.pending.len() {
            let ev = self.pending[self.pending_idx];

            // Check buffer capacity BEFORE advancing the index, so the
            // caller can retry with a larger buffer without losing the event.
            let needs_buf = matches!(
                ev.kind,
                EventKind::Unhandled { .. }
                    | EventKind::VideoFrame { .. }
                    | EventKind::AudioFrame { .. }
                    | EventKind::SnapshotData
            );
            if needs_buf && ev.body_len > buf.len() {
                return Err(BcError::BufferTooSmall {
                    needed: ev.body_len,
                    available: buf.len(),
                });
            }

            self.pending_idx += 1;

            let output_event = match ev.kind {
                EventKind::SessionTimeout => Event::SessionTimeout,
                EventKind::Pong => Event::Pong,
                EventKind::CommandCompleted {
                    msg_id,
                    msg_num,
                    status,
                } => Event::CommandCompleted {
                    msg_id,
                    msg_num,
                    status,
                },
                EventKind::CommandFailed {
                    msg_id,
                    msg_num,
                    status,
                } => Event::CommandFailed {
                    msg_id,
                    msg_num,
                    status,
                },
                EventKind::Unhandled { msg_id } => {
                    if ev.body_len > 0 {
                        buf[..ev.body_len].copy_from_slice(
                            &self.recv_buf[ev.body_start..ev.body_start + ev.body_len],
                        );
                    }
                    Event::UnhandledMessage {
                        msg_id,
                        body: &buf[..ev.body_len],
                    }
                }
                EventKind::LoggedIn => {
                    let result = self
                        .login_result
                        .take()
                        .ok_or(BcError::Protocol("missing login result"))?;
                    Event::LoggedIn(result)
                }
                EventKind::LoginFailed { status } => Event::LoginFailed(status),
                EventKind::MediaInfo { stream_id, info } => {
                    Event::StreamMetadata { stream_id, info }
                }
                EventKind::VideoFrame {
                    stream_id,
                    channel,
                    is_keyframe,
                    codec,
                    microseconds,
                } => {
                    let src = if ev.from_media {
                        &self.media_out
                    } else {
                        &*self.recv_buf
                    };
                    buf[..ev.body_len]
                        .copy_from_slice(&src[ev.body_start..ev.body_start + ev.body_len]);
                    Event::VideoFrame {
                        stream_id,
                        channel,
                        is_keyframe,
                        codec,
                        data: &buf[..ev.body_len],
                        microseconds,
                    }
                }
                EventKind::AudioFrame { stream_id, codec } => {
                    let src = if ev.from_media {
                        &self.media_out
                    } else {
                        &*self.recv_buf
                    };
                    buf[..ev.body_len]
                        .copy_from_slice(&src[ev.body_start..ev.body_start + ev.body_len]);
                    Event::AudioFrame {
                        stream_id,
                        codec,
                        data: &buf[..ev.body_len],
                    }
                }
                EventKind::SnapshotData => {
                    let src = if ev.from_media {
                        &self.media_out
                    } else {
                        &*self.recv_buf
                    };
                    buf[..ev.body_len]
                        .copy_from_slice(&src[ev.body_start..ev.body_start + ev.body_len]);
                    Event::SnapshotData {
                        data: &buf[..ev.body_len],
                    }
                }
                EventKind::SnapshotFailed { status } => Event::SnapshotFailed { status },
                EventKind::TalkResponse(kind) => {
                    let body = &self.recv_buf[ev.body_start..ev.body_start + ev.body_len];
                    Event::Talk(crate::talk::parse_response(kind, body)?)
                }
                EventKind::StreamStarted => Event::StreamStarted,
                EventKind::StreamSubscribed {
                    stream_id,
                    channel,
                    stream_type,
                } => Event::StreamSubscribed {
                    stream_id,
                    channel,
                    stream_type,
                },
                EventKind::StreamUnsubscribed { stream_id } => {
                    Event::StreamUnsubscribed { stream_id }
                }
                EventKind::StreamStopped => Event::StreamStopped,
                EventKind::DeviceResponse(dk) => {
                    let body = &self.recv_buf[ev.body_start..ev.body_start + ev.body_len];
                    Event::Device(crate::device::parse_response(dk, body)?)
                }
                EventKind::VideoResponse(vk) => {
                    let body = &self.recv_buf[ev.body_start..ev.body_start + ev.body_len];
                    Event::Video(crate::video_cfg::parse_response(vk, body)?)
                }
                EventKind::NetworkResponse(nk) => {
                    let body = &self.recv_buf[ev.body_start..ev.body_start + ev.body_len];
                    Event::Network(crate::network_cfg::parse_response(nk, body)?)
                }
                EventKind::PtzResponse(pk) => {
                    let body = &self.recv_buf[ev.body_start..ev.body_start + ev.body_len];
                    Event::Ptz(crate::ptz::parse_response(pk, body)?)
                }
                EventKind::AlarmResponse(ak) => {
                    let body = &self.recv_buf[ev.body_start..ev.body_start + ev.body_len];
                    Event::Alarm(crate::alarm::parse_response(ak, body)?)
                }
                EventKind::RecordingResponse(rk) => {
                    let body = &self.recv_buf[ev.body_start..ev.body_start + ev.body_len];
                    Event::Recording(crate::recording::parse_response(rk, body)?)
                }
                EventKind::NotificationResponse(nk) => {
                    let body = &self.recv_buf[ev.body_start..ev.body_start + ev.body_len];
                    Event::Notification(crate::notification::parse_response(nk, body)?)
                }
                EventKind::FileData => {
                    buf[..ev.body_len].copy_from_slice(
                        &self.recv_buf[ev.body_start..ev.body_start + ev.body_len],
                    );
                    Event::FileData {
                        data: &buf[..ev.body_len],
                    }
                }
                EventKind::ThumbnailData => {
                    buf[..ev.body_len].copy_from_slice(
                        &self.recv_buf[ev.body_start..ev.body_start + ev.body_len],
                    );
                    Event::ThumbnailData {
                        data: &buf[..ev.body_len],
                    }
                }
            };

            // If all pending events drained, clean up
            if self.pending_idx >= self.pending.len() {
                self.pending.clear();
                self.pending_idx = 0;
                self.media_out.clear();
            }

            return Ok(Output::Event(output_event));
        }

        // 3. Nothing pending -- return next timeout deadline
        Ok(Output::Timeout(self.next_deadline()))
    }

    fn handle_timeout(&mut self, now: Instant) -> Result<(), BcError> {
        self.check_relogin(now)?;
        self.check_keepalive(now)?;
        self.check_stream_watchdog(now);
        Ok(())
    }

    fn handle_tcp_data(&mut self, now: Instant, data: &[u8]) -> Result<(), BcError> {
        self.last_recv = now;

        // Try to compact recv buffer to make room
        self.try_compact_recv();

        // Check space
        if self.recv_end + data.len() > self.recv_buf.len() {
            return Err(BcError::MessageTooLarge {
                size: self.recv_end + data.len(),
                max: self.recv_buf.len(),
            });
        }

        // Copy incoming data
        self.recv_buf[self.recv_end..self.recv_end + data.len()].copy_from_slice(data);
        self.recv_end += data.len();

        // Parse complete messages
        self.parse_messages()
    }

    fn handle_command(&mut self, cmd: Command) -> Result<(), BcError> {
        match cmd {
            Command::Ping => self.send_ping(),
            Command::Login(params) => self.start_login(params),
            Command::Logout => self.send_logout(),
            Command::StartStream(req) => self.send_stream_start(req, 0),
            Command::SubscribeStream(sub) => self.subscribe_stream(sub),
            Command::UnsubscribeStream { stream_id } => self.unsubscribe_stream(stream_id),
            Command::StopStream(stop) => self.send_stream_stop(stop),
            Command::Snapshot(req) => self.send_snapshot(req),
            Command::OpenTalkback { channel } => self.open_talkback(channel),
            Command::CloseTalkback { channel } => self.close_talkback(channel),
            Command::Talk(command) => self.send_talk_command(&command),
            Command::Device(dc) => self.send_device_command(&dc),
            Command::Video(vc) => self.send_video_command(&vc),
            Command::Network(nc) => self.send_network_command(&nc),
            Command::Ptz(pc) => self.send_ptz_command(&pc),
            Command::Alarm(ac) => self.send_alarm_command(&ac),
            Command::Recording(rc) => self.send_recording_command(&rc),
            Command::Notification(nc) => self.send_notification_command(&nc),
        }
    }

    fn parse_messages(&mut self) -> Result<(), BcError> {
        loop {
            let data_len = self.recv_end - self.recv_start;
            if data_len < HEADER_LEN_SHORT {
                break;
            }

            let data = &self.recv_buf[self.recv_start..self.recv_end];

            // Check for magic
            if !has_header_magic(data) {
                if let Some(offset) = scan_for_magic(data) {
                    self.stats.resync_skipped_bytes += offset as u64;
                    self.recv_start += offset;
                    continue;
                } else {
                    // Keep last 3 bytes (partial magic match)
                    let keep = data.len().min(3);
                    self.recv_start = self.recv_end - keep;
                    break;
                }
            }

            // Parse header
            let (header, header_len) = match PacketHeader::parse(data) {
                Ok(v) => v,
                Err(BcError::Incomplete) => break,
                Err(e) => return Err(e),
            };

            let total = header_len + header.body_len as usize;
            if data.len() < total {
                break; // need more data
            }

            let body_start = self.recv_start + header_len;
            let body_len = header.body_len as usize;
            self.recv_start += total;

            // Dispatch the complete message
            self.dispatch_message(header, body_start, body_len)?;
        }
        Ok(())
    }

    fn dispatch_message(
        &mut self,
        header: PacketHeader,
        body_start: usize,
        body_len: usize,
    ) -> Result<(), BcError> {
        if self.pending.is_full() {
            return Err(BcError::Protocol("pending event queue full"));
        }
        let (payload_start, payload_len) = payload_range(header, body_start, body_len)?;
        let pending_command = self.take_pending_command(header);

        // Check if the body starts with a media frame magic BEFORE decryption.
        // Binary media frames are sent unencrypted even when the session uses
        // BCEncrypt/AES for XML messages. Decrypting them would corrupt the data.
        // The frame may be preceded by the previous frame's alignment padding when
        // that padding spilled into this message.
        let media_offset = media_frame_offset(&self.recv_buf[body_start..body_start + body_len]);
        let is_media_body = media_offset.is_some();
        if media_offset.is_some_and(|(offset, _)| offset > 0) {
            self.stats.padded_media_bodies += 1;
        }

        // Skip decryption for:
        // 1. Login messages (handled separately in dispatch_login)
        // 2. Stream messages with media frame magic (binary video/audio data)
        //    — UNLESS FullAes is negotiated, where media is also encrypted
        // 3. Stream continuation chunks (raw video data while accumulating)
        //    — same FullAes exception
        // Note: stream XML ack responses ARE encrypted and must be decrypted.
        let full_aes = matches!(self.encryption, EncryptionMode::FullAes);
        let skip_decrypt = header.msg_id == crate::COMMAND_LOGIN
            || (is_media_body && !full_aes)
            || (header.msg_id == crate::COMMAND_SNAP && header.is_binary() && !full_aes)
            || (header.msg_id == crate::COMMAND_STREAM
                && !(self.video_accums.is_empty()
                    && self.audio_accums.is_empty()
                    && self.header_carry.is_empty())
                && !full_aes);
        if body_len > 0 && !skip_decrypt {
            let channel_id = (header.encryption_offset & 0xFF) as u8;
            if header.msg_id == crate::COMMAND_SNAP
                && !header.is_binary()
                && !full_aes
                && self.snapshot_accum.is_some()
                && header.extension.unwrap_or(0) == 0
            {
            } else if header.msg_id == crate::COMMAND_SNAP
                && !header.is_binary()
                && !full_aes
                && let Some(offset) = header.extension
                && offset > 0
                && offset < body_len as u32
            {
                self.decrypt_body(body_start, offset as usize, channel_id, None);
            } else {
                self.decrypt_body(body_start, body_len, channel_id, header.extension);
            }
        }

        if header.msg_id != crate::COMMAND_LOGIN
            && !response_status_is_success(header.response_code())
        {
            if header.msg_id == crate::COMMAND_SNAP {
                self.fail_snapshot(header.response_code());
            } else {
                self.push_command_failure(header, pending_command);
            }
            return Ok(());
        }

        if header.msg_id == crate::COMMAND_PING
            && matches!(pending_command, Some(PendingCommandKind::Ping))
        {
            self.pending_ping = None;
            self.missed_pings = 0;
            self.pending.push(PendingEvent::new(EventKind::Pong, 0, 0));
            return Ok(());
        }

        match header.msg_id {
            crate::COMMAND_UDP_KEEP_ALIVE if header.is_modern() => {
                self.reply_udp_keepalive(header)?;
            }
            crate::COMMAND_LOGIN => {
                if let Some(kind) = self.dispatch_login(header, body_start, body_len)? {
                    self.pending
                        .push(PendingEvent::new(kind, body_start, body_len));
                }
            }
            crate::COMMAND_STREAM => {
                self.stats.stream_messages += 1;
                let stream_id = self.stream_id_from_header(header.encryption_offset);
                self.dispatch_stream(body_start, body_len, stream_id, header.extension)?;
            }
            crate::COMMAND_PREVIEW_STOP if header.is_modern() => {
                self.active_streams = self.active_streams.saturating_sub(1);
                self.pending.push(PendingEvent::new(
                    EventKind::StreamStopped,
                    body_start,
                    body_len,
                ));
            }
            crate::COMMAND_SNAP => {
                self.dispatch_snapshot(header, body_start, body_len)?;
            }
            crate::COMMAND_FILE_READ | crate::COMMAND_COVER_FILE_READ if header.is_binary() => {
                self.pending
                    .push(PendingEvent::new(EventKind::FileData, body_start, body_len));
            }
            crate::COMMAND_RECORD_THUMBNAIL
            | crate::COMMAND_COVER_THUMBNAIL
            | crate::COMMAND_COVER_THUMBNAIL_V2
                if header.is_binary() =>
            {
                self.pending.push(PendingEvent::new(
                    EventKind::ThumbnailData,
                    body_start,
                    body_len,
                ));
            }
            msg_id if header.is_modern() => {
                // Try domain classifiers before falling through to Unhandled
                if let Some(talk) = crate::talk::classify_response(msg_id) {
                    self.pending.push(PendingEvent::new(
                        EventKind::TalkResponse(talk),
                        payload_start,
                        payload_len,
                    ));
                } else if let Some(dk) = crate::device::classify_response(msg_id) {
                    self.pending.push(PendingEvent::new(
                        EventKind::DeviceResponse(dk),
                        payload_start,
                        payload_len,
                    ));
                } else if let Some(vk) = crate::video_cfg::classify_response(msg_id) {
                    self.pending.push(PendingEvent::new(
                        EventKind::VideoResponse(vk),
                        payload_start,
                        payload_len,
                    ));
                } else if let Some(nk) = crate::network_cfg::classify_response(msg_id) {
                    self.pending.push(PendingEvent::new(
                        EventKind::NetworkResponse(nk),
                        payload_start,
                        payload_len,
                    ));
                } else if let Some(pk) = crate::ptz::classify_response(msg_id) {
                    self.pending.push(PendingEvent::new(
                        EventKind::PtzResponse(pk),
                        payload_start,
                        payload_len,
                    ));
                } else if let Some(ak) = crate::alarm::classify_response(msg_id) {
                    self.pending.push(PendingEvent::new(
                        EventKind::AlarmResponse(ak),
                        payload_start,
                        payload_len,
                    ));
                } else if let Some(rk) = crate::recording::classify_response(msg_id) {
                    self.pending.push(PendingEvent::new(
                        EventKind::RecordingResponse(rk),
                        payload_start,
                        payload_len,
                    ));
                } else if let Some(nk) = crate::notification::classify_response(msg_id) {
                    self.pending.push(PendingEvent::new(
                        EventKind::NotificationResponse(nk),
                        payload_start,
                        payload_len,
                    ));
                } else {
                    self.pending.push(PendingEvent::new(
                        EventKind::Unhandled { msg_id },
                        body_start,
                        body_len,
                    ));
                }
            }
            _ => {
                self.pending.push(PendingEvent::new(
                    EventKind::Unhandled {
                        msg_id: header.msg_id,
                    },
                    body_start,
                    body_len,
                ));
            }
        }

        if matches!(pending_command, Some(PendingCommandKind::Generic)) && !self.pending.is_full() {
            self.pending.push(PendingEvent::new(
                EventKind::CommandCompleted {
                    msg_id: header.msg_id,
                    msg_num: header.message_number(),
                    status: header.response_code(),
                },
                0,
                0,
            ));
        }

        Ok(())
    }

    fn dispatch_snapshot(
        &mut self,
        header: PacketHeader,
        body_start: usize,
        body_len: usize,
    ) -> Result<(), BcError> {
        if header.is_binary() {
            if body_len > crate::MAX_SNAPSHOT_BYTES {
                return Err(BcError::Protocol("snapshot payload exceeds maximum size"));
            }
            self.clear_pending_snapshot();
            self.snapshot_in_flight = false;
            self.snapshot_accum = None;
            self.pending.push(PendingEvent::new(
                EventKind::SnapshotData,
                body_start,
                body_len,
            ));
            return Ok(());
        }

        if body_len == 0 && self.snapshot_in_flight && matches!(header.response_code(), 0 | 200) {
            return Ok(());
        }

        let payload_offset = header.extension.unwrap_or(0) as usize;
        if payload_offset > body_len {
            return Err(BcError::Protocol(
                "snapshot payload offset exceeds body length",
            ));
        }

        let extension_end = body_start + payload_offset;
        let payload_start = extension_end;
        let payload_len = body_len - payload_offset;
        let extension = &self.recv_buf[body_start..extension_end];
        let payload = &self.recv_buf[payload_start..payload_start + payload_len];

        if snapshot_extension_is_binary(extension)? {
            if !self.snapshot_in_flight {
                return Err(BcError::Protocol("snapshot data arrived without a request"));
            }
            let complete = self.append_snapshot_payload(payload_start, payload_len)?;
            return self.finish_snapshot_if_ready(header.response_code(), complete);
        }

        if !extension.is_empty() && extension.starts_with(b"<") {
            let expected_data_len = parse_snapshot_size(extension)?;
            self.start_snapshot(expected_data_len);
            let complete = self.append_snapshot_payload(payload_start, payload_len)?;
            self.finish_snapshot_if_ready(header.response_code(), complete)?;
        } else if payload.starts_with(b"<") {
            let expected_data_len = parse_snapshot_size(payload)?;
            self.start_snapshot(expected_data_len);
        } else {
            let complete = self.append_snapshot_payload(payload_start, payload_len)?;
            self.finish_snapshot_if_ready(header.response_code(), complete)?;
        }

        Ok(())
    }

    fn start_snapshot(&mut self, expected_data_len: usize) {
        self.snapshot_accum = Some(SnapshotAccum {
            expected_data_len,
            data: Vec::with_capacity(expected_data_len),
        });
    }

    fn append_snapshot_payload(
        &mut self,
        payload_start: usize,
        payload_len: usize,
    ) -> Result<bool, BcError> {
        let complete = {
            let payload = &self.recv_buf[payload_start..payload_start + payload_len];
            let snapshot = self
                .snapshot_accum
                .as_mut()
                .ok_or(BcError::Protocol("snapshot data arrived without metadata"))?;
            let received_len = snapshot
                .data
                .len()
                .checked_add(payload.len())
                .ok_or(BcError::Protocol("snapshot payload length overflow"))?;
            if received_len > snapshot.expected_data_len {
                return Err(BcError::Protocol("snapshot payload exceeds metadata size"));
            }
            snapshot.data.extend_from_slice(payload);
            received_len == snapshot.expected_data_len
        };
        Ok(complete)
    }

    fn finish_snapshot_if_ready(&mut self, status: u16, complete: bool) -> Result<(), BcError> {
        if status == 200 || (status == 0 && !complete) {
            return Ok(());
        }
        if !complete {
            return Err(BcError::Protocol(
                "snapshot ended before its advertised byte count",
            ));
        }
        let snapshot = self
            .snapshot_accum
            .take()
            .expect("snapshot accumulator exists after successful append");
        let media_start = self.media_out.len();
        let data_len = snapshot.data.len();
        self.media_out.extend_from_slice(&snapshot.data);
        self.pending.push(PendingEvent::media(
            EventKind::SnapshotData,
            media_start,
            data_len,
        ));
        self.snapshot_in_flight = false;
        self.clear_pending_snapshot();
        Ok(())
    }

    fn fail_snapshot(&mut self, status: u16) {
        self.clear_pending_snapshot();
        self.snapshot_in_flight = false;
        self.snapshot_accum = None;
        self.pending.push(PendingEvent::new(
            EventKind::SnapshotFailed { status },
            0,
            0,
        ));
    }

    fn clear_pending_snapshot(&mut self) {
        self.pending_commands
            .retain(|_, kind| *kind != PendingCommandKind::Snapshot);
    }

    /// Route a COMMAND_STREAM message body: detect media frames, XML acks,
    /// or continuation data for an in-progress chunked video frame.
    fn dispatch_stream(
        &mut self,
        body_start: usize,
        body_len: usize,
        stream_id: u32,
        payload_offset: Option<u32>,
    ) -> Result<(), BcError> {
        if body_len == 0 {
            return Ok(());
        }

        // A header that ran past the end of the previous message must be completed
        // before anything else: the bytes in front of this body are its remainder.
        if !self.header_carry.is_empty() {
            return self.resume_header_carry(body_start, body_len, stream_id, payload_offset);
        }

        // Check for media frame magic at start of body. A new frame outranks the
        // continuation accumulator: alignment padding from the previous frame can
        // precede it, and appending it as continuation data would corrupt whichever
        // frame is still accumulating.
        if let Some((offset, magic)) =
            media_frame_offset(&self.recv_buf[body_start..body_start + body_len])
        {
            return self.dispatch_media_start(
                magic,
                body_start + offset,
                body_len - offset,
                stream_id,
            );
        }

        // If we're accumulating a chunked video frame, continuation data takes
        // priority — raw H264 bytes can start with 0x3C ('<', NAL FU-A) which
        // would otherwise be misidentified as XML.
        if let Some(key) = self.pending_audio_key() {
            return self.append_audio_continuation(
                key,
                body_start,
                body_len,
                stream_id,
                payload_offset,
            );
        }
        let accum_key = if self.video_accums.contains_key(&stream_id) {
            Some(stream_id)
        } else if self.video_accums.len() == 1 {
            self.video_accums.keys().next().copied()
        } else {
            if !self.video_accums.is_empty() {
                self.stats.continuation_ambiguous += 1;
                tracing::debug!(
                    stream_id,
                    accums = ?self.video_accums.keys().collect::<Vec<_>>(),
                    body_len,
                    "continuation chunk could not be attributed",
                );
            }
            None
        };
        if let Some(accum_key) = accum_key {
            let accum = self.video_accums.get_mut(&accum_key).unwrap();
            let needed = accum.expected_data_len.saturating_sub(accum.data.len());
            let padding = padding_len(accum.expected_data_len);
            let to_copy = body_len.min(needed);
            accum
                .data
                .extend_from_slice(&self.recv_buf[body_start..body_start + to_copy]);
            self.stats.continuation_chunks += 1;
            self.try_complete_video_accum(accum_key)?;

            // If the continuation chunk had more data than needed to complete
            // the frame, the excess contains the next frame(s). Dispatch them.
            if to_copy < body_len {
                let padding_to_skip = padding.min(body_len - to_copy);
                let rest_start = body_start + to_copy + padding_to_skip;
                let rest_len = body_len - to_copy - padding_to_skip;
                if rest_len > 0 {
                    return self.dispatch_stream(rest_start, rest_len, stream_id, payload_offset);
                }
            }
            return Ok(());
        }

        // Check for XML ack (e.g. stream started response) — only when NOT
        // accumulating a chunked frame.
        if self.recv_buf[body_start] == b'<' {
            match self.maybe_update_stream_handle_mapping(body_start, body_len, payload_offset) {
                Ok(()) => {}
                Err(BcError::XmlParse(_)) => {
                    // Camera may send Extension XML with binary payload — not fatal.
                }
                Err(e) => return Err(e),
            }
            if !self.pending.is_full() {
                self.pending.push(PendingEvent::new(
                    EventKind::StreamStarted,
                    body_start,
                    body_len,
                ));
            }
            self.active_streams = self.active_streams.saturating_add(1);
            return Ok(());
        }

        if let Some(offset) = find_media_magic(&self.recv_buf[body_start..body_start + body_len]) {
            return self.dispatch_stream(
                body_start + offset,
                body_len - offset,
                stream_id,
                payload_offset,
            );
        }

        self.stats.stream_bodies_unrecognized += 1;
        tracing::debug!(
            body_len,
            stream_id,
            accums = self.video_accums.len(),
            head = ?&self.recv_buf[body_start..body_start + body_len.min(24)],
            "discarding unrecognised stream body",
        );
        Ok(())
    }

    /// Handle a COMMAND_STREAM body that starts with a known media frame magic.
    /// Parses the frame header and either emits the frame immediately (if it fits
    /// in this single message) or starts accumulation for chunked delivery.
    fn dispatch_media_start(
        &mut self,
        magic: crate::media::MediaMagic,
        body_start: usize,
        body_len: usize,
        stream_id: u32,
    ) -> Result<(), BcError> {
        use crate::media::MediaMagic;

        let body = &self.recv_buf[body_start..body_start + body_len];

        match magic {
            MediaMagic::InfoV1 | MediaMagic::InfoV2 => {
                let (info, consumed) = match crate::media::parse_stream_metadata(body) {
                    Ok(v) => v,
                    Err(BcError::Incomplete) => {
                        self.stash_header_carry(body_start, body_len);
                        return Ok(());
                    }
                    Err(e) => return Err(e),
                };
                self.last_stream_metadata = Some(info);
                if !self.pending.is_full() {
                    self.pending.push(PendingEvent::new(
                        EventKind::MediaInfo { stream_id, info },
                        0,
                        0,
                    ));
                }
                // Check for additional frames after the info header
                let aligned = crate::media::align8(consumed);
                if aligned < body_len {
                    self.dispatch_remaining_media(
                        body_start + aligned,
                        body_len - aligned,
                        stream_id,
                    )?;
                }
            }
            MediaMagic::IFrame(channel) | MediaMagic::PFrame(channel) => {
                let is_keyframe = matches!(magic, MediaMagic::IFrame(_));
                let (codec, data_len, microseconds, header_total, stream_handle_hint) =
                    match crate::media::parse_video_header(body) {
                        Ok(v) => v,
                        Err(BcError::Incomplete) => {
                            self.stash_header_carry(body_start, body_len);
                            return Ok(());
                        }
                        Err(e) => return Err(e),
                    };

                // parse_video_header only needs 24 bytes but reports a header that
                // includes the variable-length extension, which can land in the next
                // message. Consuming it here would splice header bytes into the payload.
                if body_len < header_total {
                    self.stash_header_carry(body_start, body_len);
                    return Ok(());
                }

                let resolved_stream_id =
                    self.resolve_video_stream_id(stream_id, stream_handle_hint, channel, codec);
                let accum_key = if stream_id != 0 {
                    stream_id
                } else {
                    resolved_stream_id
                };
                self.discard_accums(accum_key);

                let available_after_header = body_len.saturating_sub(header_total);

                if available_after_header >= data_len as usize {
                    // Complete video frame fits in this single message
                    self.emit_video_frame(
                        resolved_stream_id,
                        channel,
                        is_keyframe,
                        codec,
                        microseconds,
                        body_start + header_total,
                        data_len as usize,
                    );

                    // Check for additional frames after this one in the same body
                    let frame_end =
                        header_total + data_len as usize + padding_len(data_len as usize);
                    if frame_end < body_len {
                        self.dispatch_remaining_media(
                            body_start + frame_end,
                            body_len - frame_end,
                            resolved_stream_id,
                        )?;
                    }
                } else {
                    // Partial video frame — start accumulating chunks.
                    // The camera splits large frames across multiple BC messages.
                    // Subsequent messages carry raw continuation bytes (no header).
                    let data_start = body_start + header_total;
                    let data_avail = available_after_header;
                    let mut data = Vec::with_capacity(data_len as usize);
                    data.extend_from_slice(&self.recv_buf[data_start..data_start + data_avail]);

                    self.video_accums.insert(
                        accum_key,
                        VideoAccum {
                            stream_id: resolved_stream_id,
                            channel,
                            is_keyframe,
                            codec,
                            microseconds,
                            expected_data_len: data_len as usize,
                            data,
                        },
                    );
                    self.stats.video_accum_started += 1;
                }
            }
            MediaMagic::AacAudio | MediaMagic::AdpcmAudio => {
                let parsed = if matches!(magic, MediaMagic::AacAudio) {
                    crate::media::parse_aac_header(body).map(|(data_len, header_len)| {
                        (crate::media::AudioCodec::Aac, data_len, header_len)
                    })
                } else {
                    crate::media::parse_adpcm_header(body).map(|(data_len, header_len)| {
                        (crate::media::AudioCodec::Adpcm, data_len, header_len)
                    })
                };
                let (codec, data_len, header_len) = match parsed {
                    Ok(v) => v,
                    Err(BcError::Incomplete) => {
                        self.stash_header_carry(body_start, body_len);
                        return Ok(());
                    }
                    Err(e) => return Err(e),
                };
                self.discard_accums(stream_id);

                let frame_end = header_len + data_len;
                if body_len >= frame_end {
                    self.emit_audio_frame(stream_id, codec, body_start + header_len, data_len);
                    let aligned = crate::media::align8(frame_end);
                    if aligned < body_len {
                        self.dispatch_remaining_media(
                            body_start + aligned,
                            body_len - aligned,
                            stream_id,
                        )?;
                    }
                } else {
                    // Audio frames span messages just like video frames; dropping the
                    // partial frame would leave its tail to desynchronise the next message.
                    let mut data = Vec::with_capacity(data_len);
                    data.extend_from_slice(
                        &self.recv_buf[body_start + header_len..body_start + body_len],
                    );
                    self.audio_accums.insert(
                        stream_id,
                        AudioAccum {
                            stream_id,
                            codec,
                            expected_data_len: data_len,
                            padding: crate::media::align8(frame_end) - frame_end,
                            data,
                        },
                    );
                    self.stats.audio_accum_started += 1;
                }
            }
        }
        Ok(())
    }

    /// Parse additional media frames from remaining bytes in a message body
    /// after a complete frame has been consumed.
    fn dispatch_remaining_media(
        &mut self,
        body_start: usize,
        body_len: usize,
        stream_id: u32,
    ) -> Result<(), BcError> {
        if body_len < 4 || self.pending.is_full() {
            return Ok(());
        }

        let magic_u32 = u32::from_le_bytes([
            self.recv_buf[body_start],
            self.recv_buf[body_start + 1],
            self.recv_buf[body_start + 2],
            self.recv_buf[body_start + 3],
        ]);
        if let Some(magic) = crate::media::MediaMagic::from_u32(magic_u32) {
            self.dispatch_media_start(magic, body_start, body_len, stream_id)?;
        } else {
            self.stats.trailing_bytes_unrecognized += 1;
        }
        // If no magic found, remaining bytes are padding or unknown data

        Ok(())
    }

    /// Copy completed video frame data to media_out and push a PendingEvent.
    #[allow(clippy::too_many_arguments)]
    fn emit_video_frame(
        &mut self,
        stream_id: u32,
        channel: u8,
        is_keyframe: bool,
        codec: crate::media::VideoCodec,
        microseconds: u32,
        data_start: usize,
        data_len: usize,
    ) {
        if data_len == 0 {
            return;
        }
        if self.pending.is_full() {
            self.stats.pending_drops += 1;
            return;
        }
        let media_start = self.media_out.len();
        self.media_out
            .extend_from_slice(&self.recv_buf[data_start..data_start + data_len]);
        self.pending.push(PendingEvent::media(
            EventKind::VideoFrame {
                stream_id,
                channel,
                is_keyframe,
                codec,
                microseconds,
            },
            media_start,
            data_len,
        ));
        self.stats.video_frames_emitted += 1;
    }
    fn emit_audio_frame(
        &mut self,
        stream_id: u32,
        codec: crate::media::AudioCodec,
        data_start: usize,
        data_len: usize,
    ) {
        if data_len == 0 {
            return;
        }
        if self.pending.is_full() {
            self.stats.pending_drops += 1;
            return;
        }
        let media_start = self.media_out.len();
        self.media_out
            .extend_from_slice(&self.recv_buf[data_start..data_start + data_len]);
        self.pending.push(PendingEvent::media(
            EventKind::AudioFrame { stream_id, codec },
            media_start,
            data_len,
        ));
        self.stats.audio_frames_emitted += 1;
    }

    /// A new frame magic means any partially received frame for that stream is lost.
    fn discard_accums(&mut self, key: u32) {
        if self.video_accums.remove(&key).is_some() {
            self.stats.video_accum_abandoned += 1;
        }
        self.audio_accums.remove(&key);
    }

    /// Retain a frame header that ran past the end of its message so it can be
    /// completed once the next message arrives.
    fn stash_header_carry(&mut self, body_start: usize, body_len: usize) {
        if body_len == 0 || body_len > MAX_MEDIA_HEADER {
            self.stats.stream_bodies_unrecognized += 1;
            return;
        }
        self.header_carry.clear();
        self.header_carry
            .extend_from_slice(&self.recv_buf[body_start..body_start + body_len]);
        self.stats.split_headers += 1;
    }

    /// Complete a frame header that straddled a message boundary, then hand the
    /// rest of the body back to the normal continuation path.
    fn resume_header_carry(
        &mut self,
        body_start: usize,
        body_len: usize,
        stream_id: u32,
        payload_offset: Option<u32>,
    ) -> Result<(), BcError> {
        use crate::media::MediaMagic;

        let mut carry = std::mem::take(&mut self.header_carry);
        let carry_len = carry.len();
        let take = body_len.min(MAX_MEDIA_HEADER.saturating_sub(carry_len));
        carry.extend_from_slice(&self.recv_buf[body_start..body_start + take]);

        let Some((_, magic)) = media_frame_offset(&carry) else {
            self.stats.stream_bodies_unrecognized += 1;
            return Ok(());
        };

        match magic {
            MediaMagic::IFrame(channel) | MediaMagic::PFrame(channel) => {
                let (codec, data_len, microseconds, header_total, stream_handle_hint) =
                    match crate::media::parse_video_header(&carry) {
                        Ok(v) => v,
                        Err(BcError::Incomplete) => return self.keep_carry(carry),
                        Err(e) => return Err(e),
                    };
                if header_total > MAX_MEDIA_HEADER {
                    self.stats.stream_bodies_unrecognized += 1;
                    return Ok(());
                }
                if carry.len() < header_total {
                    return self.keep_carry(carry);
                }

                let resolved_stream_id =
                    self.resolve_video_stream_id(stream_id, stream_handle_hint, channel, codec);
                let accum_key = if stream_id != 0 {
                    stream_id
                } else {
                    resolved_stream_id
                };
                self.discard_accums(accum_key);
                self.video_accums.insert(
                    accum_key,
                    VideoAccum {
                        stream_id: resolved_stream_id,
                        channel,
                        is_keyframe: matches!(magic, MediaMagic::IFrame(_)),
                        codec,
                        microseconds,
                        expected_data_len: data_len as usize,
                        data: Vec::with_capacity(data_len as usize),
                    },
                );
                self.stats.video_accum_started += 1;

                let consumed = header_total - carry_len;
                self.dispatch_stream(
                    body_start + consumed,
                    body_len - consumed,
                    stream_id,
                    payload_offset,
                )
            }
            MediaMagic::AacAudio | MediaMagic::AdpcmAudio => {
                let parsed = if matches!(magic, MediaMagic::AacAudio) {
                    crate::media::parse_aac_header(&carry).map(|(data_len, header_len)| {
                        (crate::media::AudioCodec::Aac, data_len, header_len)
                    })
                } else {
                    crate::media::parse_adpcm_header(&carry).map(|(data_len, header_len)| {
                        (crate::media::AudioCodec::Adpcm, data_len, header_len)
                    })
                };
                let (codec, data_len, header_len) = match parsed {
                    Ok(v) => v,
                    Err(BcError::Incomplete) => return self.keep_carry(carry),
                    Err(e) => return Err(e),
                };

                self.discard_accums(stream_id);
                let frame_end = header_len + data_len;
                self.audio_accums.insert(
                    stream_id,
                    AudioAccum {
                        stream_id,
                        codec,
                        expected_data_len: data_len,
                        padding: crate::media::align8(frame_end) - frame_end,
                        data: Vec::with_capacity(data_len),
                    },
                );
                self.stats.audio_accum_started += 1;

                let consumed = header_len - carry_len;
                self.dispatch_stream(
                    body_start + consumed,
                    body_len - consumed,
                    stream_id,
                    payload_offset,
                )
            }
            MediaMagic::InfoV1 | MediaMagic::InfoV2 => {
                let (info, consumed_total) = match crate::media::parse_stream_metadata(&carry) {
                    Ok(v) => v,
                    Err(BcError::Incomplete) => return self.keep_carry(carry),
                    Err(e) => return Err(e),
                };
                self.last_stream_metadata = Some(info);
                if !self.pending.is_full() {
                    self.pending.push(PendingEvent::new(
                        EventKind::MediaInfo { stream_id, info },
                        0,
                        0,
                    ));
                }
                let consumed = crate::media::align8(consumed_total).saturating_sub(carry_len);
                if consumed >= body_len {
                    return Ok(());
                }
                self.dispatch_stream(
                    body_start + consumed,
                    body_len - consumed,
                    stream_id,
                    payload_offset,
                )
            }
        }
    }

    fn keep_carry(&mut self, carry: Vec<u8>) -> Result<(), BcError> {
        if carry.len() < MAX_MEDIA_HEADER {
            self.header_carry = carry;
        } else {
            self.stats.stream_bodies_unrecognized += 1;
        }
        Ok(())
    }

    /// Resolve which audio accumulation a headerless continuation chunk belongs to.
    fn pending_audio_key(&self) -> Option<u32> {
        if self.audio_accums.is_empty() {
            None
        } else if self.audio_accums.len() == 1 {
            self.audio_accums.keys().next().copied()
        } else {
            None
        }
    }

    /// Append a headerless continuation chunk to a chunked audio frame, emitting
    /// it once complete and dispatching whatever follows the frame's padding.
    fn append_audio_continuation(
        &mut self,
        key: u32,
        body_start: usize,
        body_len: usize,
        stream_id: u32,
        payload_offset: Option<u32>,
    ) -> Result<(), BcError> {
        let Some(accum) = self.audio_accums.get_mut(&key) else {
            return Ok(());
        };
        let needed = accum.expected_data_len.saturating_sub(accum.data.len());
        let to_copy = body_len.min(needed);
        accum
            .data
            .extend_from_slice(&self.recv_buf[body_start..body_start + to_copy]);
        if accum.data.len() < accum.expected_data_len {
            return Ok(());
        }
        let accum = self
            .audio_accums
            .remove(&key)
            .expect("audio accumulation was just borrowed");
        let padding = accum.padding;
        self.emit_audio_accum(accum);

        if to_copy < body_len {
            let padding_to_skip = padding.min(body_len - to_copy);
            let rest_start = body_start + to_copy + padding_to_skip;
            let rest_len = body_len - to_copy - padding_to_skip;
            if rest_len > 0 {
                return self.dispatch_stream(rest_start, rest_len, stream_id, payload_offset);
            }
        }
        Ok(())
    }

    fn emit_audio_accum(&mut self, accum: AudioAccum) {
        self.stats.audio_accum_completed += 1;
        if accum.expected_data_len == 0 {
            return;
        }
        if self.pending.is_full() {
            self.stats.pending_drops += 1;
            return;
        }
        let media_start = self.media_out.len();
        self.media_out
            .extend_from_slice(&accum.data[..accum.expected_data_len]);
        self.pending.push(PendingEvent::media(
            EventKind::AudioFrame {
                stream_id: accum.stream_id,
                codec: accum.codec,
            },
            media_start,
            accum.expected_data_len,
        ));
        self.stats.audio_frames_emitted += 1;
    }

    /// Check if the in-progress video frame accumulation is complete.
    /// If so, emit it as a VideoFrame event and clear the accumulator.
    fn try_complete_video_accum(&mut self, accum_key: u32) -> Result<(), BcError> {
        let complete = self
            .video_accums
            .get(&accum_key)
            .is_some_and(|a| a.data.len() >= a.expected_data_len);

        if complete {
            let accum = self.video_accums.remove(&accum_key).unwrap();
            if !self.pending.is_full() {
                let media_start = self.media_out.len();
                self.media_out
                    .extend_from_slice(&accum.data[..accum.expected_data_len]);
                self.pending.push(PendingEvent::media(
                    EventKind::VideoFrame {
                        stream_id: accum.stream_id,
                        channel: accum.channel,
                        is_keyframe: accum.is_keyframe,
                        codec: accum.codec,
                        microseconds: accum.microseconds,
                    },
                    media_start,
                    accum.expected_data_len,
                ));
                self.stats.video_frames_emitted += 1;
                self.stats.video_accum_completed += 1;
            } else {
                self.stats.pending_drops += 1;
            }
        }

        // Safety: cap accumulation to prevent unbounded growth from corrupt data
        if self
            .video_accums
            .get(&accum_key)
            .is_some_and(|a| a.data.len() > 2 * 1024 * 1024)
        {
            self.video_accums.remove(&accum_key);
        }

        Ok(())
    }

    fn send_ping(&mut self) -> Result<(), BcError> {
        if self.role != Role::Client {
            return Err(BcError::WrongRole);
        }
        if self.pending_ping.is_some() {
            return Ok(());
        }
        let header = PacketHeader {
            msg_id: crate::COMMAND_PING,
            body_len: 0,
            encryption_offset: u32::from(self.keepalive_channel),
            status_class: make_status(BC_CLASS_MODERN_EXT, 0),
            extension: Some(0),
        };
        let msg_num = self.queue_correlated_send(header, &[], PendingCommandKind::Ping)?;
        self.pending_ping = Some(msg_num);
        Ok(())
    }

    fn start_login(&mut self, params: LoginParams) -> Result<(), BcError> {
        if self.role != Role::Client {
            return Err(BcError::WrongRole);
        }
        if !matches!(
            self.state,
            SessionState::Disconnected | SessionState::Connected
        ) {
            return Err(BcError::Protocol("login already in progress"));
        }

        self.login_params = Some(params);

        // Step 1: LoginUpgrade — header-only, no body.
        // class=LEGACY (0x6514), response_code=requested encryption mode.
        let header = PacketHeader {
            msg_id: crate::COMMAND_LOGIN,
            body_len: 0,
            encryption_offset: 0,
            status_class: make_status(BC_CLASS_LEGACY, params.encryption.to_class_value() as u16),
            extension: None,
        };
        self.queue_send(&header, &[])?;
        self.state = SessionState::AwaitingNonce;
        Ok(())
    }

    fn send_logout(&mut self) -> Result<(), BcError> {
        if self.role != Role::Client {
            return Err(BcError::WrongRole);
        }

        let header = PacketHeader {
            msg_id: crate::COMMAND_LOGOUT,
            body_len: 0,
            encryption_offset: 0,
            status_class: make_status(BC_CLASS_MODERN_SHORT, 0),
            extension: None,
        };
        self.queue_send(&header, &[])?;
        self.state = SessionState::Disconnected;
        self.pending_commands.clear();
        self.pending_ping = None;
        self.missed_pings = 0;
        self.snapshot_in_flight = false;
        self.snapshot_accum = None;
        self.encryption = EncryptionMode::None;
        self.aes_key = None;
        self.nonce = ArrayString::new();
        Ok(())
    }

    fn send_stream_start(
        &mut self,
        req: crate::stream::StreamRequest,
        msg_num: u16,
    ) -> Result<(), BcError> {
        if self.role != Role::Client {
            return Err(BcError::WrongRole);
        }
        let mut xml_buf = [0u8; crate::MAX_XML_BODY];
        let xml_len = crate::stream::build_stream_request(&req, &mut xml_buf)?;
        let header =
            crate::stream::stream_request_header(xml_len, req.channel, req.stream_type, msg_num);
        self.queue_send(&header, &xml_buf[..xml_len])
    }

    fn subscribe_stream(&mut self, sub: crate::stream::StreamSubscription) -> Result<(), BcError> {
        let stream_id = self.allocate_stream_id();
        let msg_num = self.allocate_msg_num();
        let key = stream_key(sub.channel, sub.stream_type);
        self.stream_ids_by_key.insert(key, stream_id);
        self.stream_id_by_msg_num.insert(msg_num, stream_id);
        self.stream_subs_by_id.insert(
            stream_id,
            StreamSubscriptionEntry {
                channel: sub.channel,
                stream_type: sub.stream_type,
                expected_width: sub.expected_width,
                expected_height: sub.expected_height,
            },
        );
        self.pending_subscribe_ids.push_back(stream_id);

        tracing::info!(
            stream_id, msg_num, ch = sub.channel,
            st = ?sub.stream_type,
            w = sub.expected_width, h = sub.expected_height,
            "subscribe_stream: sending start with msg_num",
        );

        if !self.pending.is_full() {
            self.pending.push(PendingEvent::new(
                EventKind::StreamSubscribed {
                    stream_id,
                    channel: sub.channel,
                    stream_type: sub.stream_type,
                },
                0,
                0,
            ));
        }

        let wire_handle = match sub.stream_type {
            crate::stream::StreamType::Main => 0,
            crate::stream::StreamType::Sub => 256,
            crate::stream::StreamType::Extern => 1024,
        };

        self.send_stream_start(
            crate::stream::StreamRequest {
                channel: sub.channel,
                handle: wire_handle,
                stream_type: sub.stream_type,
            },
            msg_num,
        )
    }

    fn unsubscribe_stream(&mut self, stream_id: u32) -> Result<(), BcError> {
        let Some(sub) = self.stream_subs_by_id.get(&stream_id).copied() else {
            return Err(BcError::Protocol("unknown stream_id"));
        };
        let default_handle = match sub.stream_type {
            crate::stream::StreamType::Main => 0,
            crate::stream::StreamType::Sub => 256,
            crate::stream::StreamType::Extern => 1024,
        };
        let handle = self
            .remote_handle_by_stream_id
            .get(&stream_id)
            .copied()
            .unwrap_or(default_handle);

        if !self.pending.is_full() {
            self.pending.push(PendingEvent::new(
                EventKind::StreamUnsubscribed { stream_id },
                0,
                0,
            ));
        }

        self.send_stream_stop(crate::stream::StreamStop {
            channel: sub.channel,
            handle,
        })?;

        self.stream_subs_by_id.remove(&stream_id);
        self.stream_ids_by_key
            .remove(&stream_key(sub.channel, sub.stream_type));
        self.pending_subscribe_ids.retain(|id| *id != stream_id);
        self.remote_handle_by_stream_id.remove(&stream_id);
        self.stream_id_by_remote_handle
            .retain(|_, id| *id != stream_id);
        self.stream_id_by_channel_codec
            .retain(|_, id| *id != stream_id);
        self.stream_id_by_msg_num.retain(|_, id| *id != stream_id);
        Ok(())
    }

    fn send_stream_stop(&mut self, stop: crate::stream::StreamStop) -> Result<(), BcError> {
        if self.role != Role::Client {
            return Err(BcError::WrongRole);
        }
        let mut xml_buf = [0u8; crate::MAX_XML_BODY];
        let xml_len = crate::stream::build_stream_stop(&stop, &mut xml_buf)?;
        let header = crate::stream::stream_stop_header(xml_len);
        self.queue_send(&header, &xml_buf[..xml_len])
    }

    fn send_snapshot(&mut self, req: crate::stream::SnapshotRequest) -> Result<(), BcError> {
        if self.role != Role::Client {
            return Err(BcError::WrongRole);
        }
        if self.snapshot_in_flight {
            return Err(BcError::Protocol("snapshot request is already in flight"));
        }
        let mut xml_buf = [0u8; crate::MAX_XML_BODY];
        let xml_len = crate::stream::build_snapshot_request(&req, &mut xml_buf)?;
        let header = crate::stream::snapshot_request_header(xml_len);
        self.queue_correlated_send(header, &xml_buf[..xml_len], PendingCommandKind::Snapshot)?;
        self.snapshot_in_flight = true;
        Ok(())
    }

    fn open_talkback(&mut self, channel: u8) -> Result<(), BcError> {
        if self.role != Role::Client {
            return Err(BcError::WrongRole);
        }
        let key = stream_key(channel, crate::stream::StreamType::Extern);
        if self.stream_ids_by_key.contains_key(&key) {
            return Err(BcError::Protocol(
                "talkback external stream is already open",
            ));
        }
        self.subscribe_stream(crate::stream::StreamSubscription {
            channel,
            stream_type: crate::stream::StreamType::Extern,
            expected_width: 0,
            expected_height: 0,
        })
    }

    fn close_talkback(&mut self, channel: u8) -> Result<(), BcError> {
        if self.role != Role::Client {
            return Err(BcError::WrongRole);
        }
        self.send_talk_command(&crate::talk::TalkCommand::Reset { channel })?;
        let stream_id = self
            .stream_ids_by_key
            .get(&stream_key(channel, crate::stream::StreamType::Extern))
            .copied()
            .ok_or(BcError::Protocol("talkback external stream is not open"))?;
        self.unsubscribe_stream(stream_id)
    }

    fn send_talk_command(&mut self, command: &crate::talk::TalkCommand) -> Result<(), BcError> {
        if self.role != Role::Client {
            return Err(BcError::WrongRole);
        }
        let channel = command.channel();
        if !self
            .stream_ids_by_key
            .contains_key(&stream_key(channel, crate::stream::StreamType::Extern))
        {
            return Err(BcError::Protocol("talkback requires an external stream"));
        }

        let mut extension = [0_u8; crate::MAX_XML_BODY];
        match command {
            crate::talk::TalkCommand::QueryAbility { .. } => {
                let extension_len = crate::talk::build_extension(channel, false, &mut extension)?;
                let header = crate::talk::command_header(
                    crate::COMMAND_TALK_CAPABILITIES,
                    channel,
                    extension_len,
                    0,
                )?;
                self.queue_correlated_send_parts(
                    header,
                    &extension[..extension_len],
                    &[],
                    false,
                    PendingCommandKind::Generic,
                )
                .map(|_| ())
            }
            crate::talk::TalkCommand::Configure(config) => {
                let extension_len = crate::talk::build_extension(channel, false, &mut extension)?;
                let mut body = [0_u8; crate::MAX_XML_BODY];
                let body_len = crate::talk::build_config(config, &mut body)?;
                let header = crate::talk::command_header(
                    crate::COMMAND_TALK_CONFIG,
                    channel,
                    extension_len,
                    body_len,
                )?;
                self.queue_correlated_send_parts(
                    header,
                    &extension[..extension_len],
                    &body[..body_len],
                    false,
                    PendingCommandKind::Generic,
                )
                .map(|_| ())
            }
            crate::talk::TalkCommand::SendAdpcm { sequence, data, .. } => {
                let extension_len = crate::talk::build_extension(channel, true, &mut extension)?;
                let body_capacity = crate::talk::adpcm_packet_capacity(data.len())?;
                let mut body = vec![0_u8; body_capacity];
                let body_len = crate::talk::build_adpcm_packet(data, *sequence, &mut body)?;
                let header = crate::talk::command_header(
                    crate::COMMAND_TALK,
                    channel,
                    extension_len,
                    body_len,
                )?
                .with_message_number(self.allocate_msg_num());
                self.queue_send_parts(
                    &header,
                    &extension[..extension_len],
                    &body[..body_len],
                    true,
                )
            }
            crate::talk::TalkCommand::Reset { .. } => {
                let extension_len = crate::talk::build_extension(channel, false, &mut extension)?;
                let header = crate::talk::command_header(
                    crate::COMMAND_TALK_RESET,
                    channel,
                    extension_len,
                    0,
                )?;
                self.queue_correlated_send_parts(
                    header,
                    &extension[..extension_len],
                    &[],
                    false,
                    PendingCommandKind::Generic,
                )
                .map(|_| ())
            }
        }
    }

    fn send_device_command(&mut self, cmd: &crate::device::DeviceCommand) -> Result<(), BcError> {
        if self.role != Role::Client {
            return Err(BcError::WrongRole);
        }
        let mut xml_buf = [0u8; crate::MAX_XML_BODY];
        let (header, xml_len) = crate::device::build_request(cmd, &mut xml_buf)?;
        self.queue_correlated_send(header, &xml_buf[..xml_len], PendingCommandKind::Generic)
            .map(|_| ())
    }

    fn send_video_command(&mut self, cmd: &crate::video_cfg::VideoCommand) -> Result<(), BcError> {
        if self.role != Role::Client {
            return Err(BcError::WrongRole);
        }
        let mut xml_buf = [0u8; crate::MAX_XML_BODY];
        let (header, xml_len) = crate::video_cfg::build_request(cmd, &mut xml_buf)?;
        self.queue_correlated_send(header, &xml_buf[..xml_len], PendingCommandKind::Generic)
            .map(|_| ())
    }

    fn send_network_command(
        &mut self,
        cmd: &crate::network_cfg::NetworkCommand,
    ) -> Result<(), BcError> {
        if self.role != Role::Client {
            return Err(BcError::WrongRole);
        }
        let mut xml_buf = [0u8; crate::MAX_XML_BODY];
        let (header, xml_len) = crate::network_cfg::build_request(cmd, &mut xml_buf)?;
        self.queue_correlated_send(header, &xml_buf[..xml_len], PendingCommandKind::Generic)
            .map(|_| ())
    }

    fn send_ptz_command(&mut self, cmd: &crate::ptz::PtzCommand) -> Result<(), BcError> {
        if self.role != Role::Client {
            return Err(BcError::WrongRole);
        }
        let mut xml_buf = [0u8; crate::MAX_XML_BODY];
        let (header, xml_len) = crate::ptz::build_request(cmd, &mut xml_buf)?;
        self.queue_correlated_send(header, &xml_buf[..xml_len], PendingCommandKind::Generic)
            .map(|_| ())
    }

    fn send_alarm_command(&mut self, cmd: &crate::alarm::AlarmCommand) -> Result<(), BcError> {
        if self.role != Role::Client {
            return Err(BcError::WrongRole);
        }
        let mut xml_buf = [0u8; crate::MAX_XML_BODY];
        let (header, xml_len) = crate::alarm::build_request(cmd, &mut xml_buf)?;
        self.queue_correlated_send(header, &xml_buf[..xml_len], PendingCommandKind::Generic)
            .map(|_| ())
    }

    fn send_recording_command(
        &mut self,
        cmd: &crate::recording::RecordingCommand,
    ) -> Result<(), BcError> {
        if self.role != Role::Client {
            return Err(BcError::WrongRole);
        }
        let mut xml_buf = [0u8; crate::MAX_XML_BODY];
        let (header, xml_len) = crate::recording::build_request(cmd, &mut xml_buf)?;
        self.queue_correlated_send(header, &xml_buf[..xml_len], PendingCommandKind::Generic)
            .map(|_| ())
    }

    fn send_notification_command(
        &mut self,
        cmd: &crate::notification::NotificationCommand,
    ) -> Result<(), BcError> {
        if self.role != Role::Client {
            return Err(BcError::WrongRole);
        }
        let mut xml_buf = [0u8; crate::MAX_XML_BODY];
        let (header, xml_len) = crate::notification::build_request(cmd, &mut xml_buf)?;
        self.queue_correlated_send(header, &xml_buf[..xml_len], PendingCommandKind::Generic)
            .map(|_| ())
    }

    fn dispatch_login(
        &mut self,
        header: PacketHeader,
        body_start: usize,
        body_len: usize,
    ) -> Result<Option<EventKind>, BcError> {
        match self.state {
            SessionState::AwaitingNonce => {
                // Step 2: camera sends nonce response (BCEncrypt-encrypted body)
                if body_len > 0 {
                    crate::encryption::bc_xor(
                        &mut self.recv_buf[body_start..body_start + body_len],
                        0,
                    );
                }
                let nonce_info =
                    auth::parse_nonce_response(&self.recv_buf[body_start..body_start + body_len])?;
                self.nonce = nonce_info.nonce;

                let mut params = self
                    .login_params
                    .ok_or(BcError::Protocol("no login params"))?;

                // Negotiate encryption: use header response_code if available,
                // otherwise fall back to XML-parsed value.
                let camera_enc = {
                    let rc = header.response_code();
                    if rc >> 8 == 0xDD {
                        match rc & 0xFF {
                            0x00 => Some(EncryptionMode::None),
                            0x01 => Some(EncryptionMode::BcEncrypt),
                            0x02 => Some(EncryptionMode::Aes),
                            0x12 => Some(EncryptionMode::FullAes),
                            _ => None,
                        }
                    } else {
                        None
                    }
                };
                let camera_max = camera_enc.unwrap_or(nonce_info.encryption);
                params.encryption = auth::negotiate_encryption(params.encryption, camera_max);
                self.login_params = Some(params);

                // Step 3: build modern login with hashed credentials
                let mut xml_buf = [0u8; crate::MAX_XML_BODY];
                let xml_len = auth::build_modern_login(&params, self.nonce.as_str(), &mut xml_buf)?;

                // BCEncrypt the body (login exchange always uses BCEncrypt)
                crate::encryption::bc_xor(&mut xml_buf[..xml_len], 0);

                let hdr = PacketHeader {
                    msg_id: crate::COMMAND_LOGIN,
                    body_len: xml_len as u32,
                    encryption_offset: 0,
                    status_class: make_status(BC_CLASS_MODERN_EXT, 0),
                    extension: Some(0),
                };
                self.queue_send(&hdr, &xml_buf[..xml_len])?;
                self.state = SessionState::AwaitingLoginConfirm;
                Ok(None)
            }
            SessionState::AwaitingLoginConfirm => {
                // Step 4: parse login confirmation (BCEncrypt-encrypted body)
                if body_len > 0 {
                    crate::encryption::bc_xor(
                        &mut self.recv_buf[body_start..body_start + body_len],
                        0,
                    );
                }
                let params = self
                    .login_params
                    .ok_or(BcError::Protocol("no login params"))?;

                match auth::parse_login_confirmation(
                    &self.recv_buf[body_start..body_start + body_len],
                    params.encryption,
                ) {
                    Ok(result) => {
                        // Set up post-login encryption state
                        self.encryption = params.encryption;
                        if matches!(
                            params.encryption,
                            EncryptionMode::Aes | EncryptionMode::FullAes
                        ) {
                            self.aes_key = Some(crate::encryption::derive_aes_key(
                                self.nonce.as_str(),
                                params.password.as_str(),
                            ));
                        }

                        self.state = SessionState::Connected;
                        self.last_login = Some(self.last_recv);
                        self.login_result = Some(result);
                        Ok(Some(EventKind::LoggedIn))
                    }
                    Err(_) => {
                        self.state = SessionState::Disconnected;
                        self.nonce = ArrayString::new();
                        Ok(Some(EventKind::LoginFailed {
                            status: header.status_class,
                        }))
                    }
                }
            }
            _ => Ok(Some(EventKind::Unhandled {
                msg_id: header.msg_id,
            })),
        }
    }

    const fn allocate_stream_id(&mut self) -> u32 {
        let id = self.next_stream_id;
        self.next_stream_id = self.next_stream_id.wrapping_add(1);
        if self.next_stream_id == 0 {
            self.next_stream_id = 1;
        }
        id
    }

    const fn allocate_msg_num(&mut self) -> u16 {
        let num = self.next_msg_num;
        self.next_msg_num = self.next_msg_num.wrapping_add(1);
        if self.next_msg_num == 0 {
            self.next_msg_num = 1;
        }
        num
    }

    fn stream_id_from_header(&self, encryption_offset: u32) -> u32 {
        let channel = (encryption_offset & 0xFF) as u8;
        let stream_type_raw = ((encryption_offset >> 8) & 0xFF) as u8;
        let msg_num = ((encryption_offset >> 16) & 0xFFFF) as u16;

        // Try msg_num routing first — most reliable when the camera echoes
        // the msg_num we set in the outgoing stream request.
        if msg_num != 0 {
            if let Some(&stream_id) = self.stream_id_by_msg_num.get(&msg_num) {
                tracing::debug!(
                    msg_num,
                    stream_id,
                    channel,
                    stream_type_raw,
                    "stream_id_from_header: routed by msg_num",
                );
                return stream_id;
            }
            tracing::debug!(
                msg_num,
                channel,
                stream_type_raw,
                "stream_id_from_header: non-zero msg_num but no mapping",
            );
        }

        // Fall back to stream_type routing
        if let Some(stream_type) = crate::stream::StreamType::from_wire_id(stream_type_raw) {
            let key = stream_key(channel, stream_type);
            if let Some(stream_id) = self.stream_ids_by_key.get(&key) {
                return *stream_id;
            }
        }
        0
    }

    fn resolve_video_stream_id(
        &mut self,
        header_stream_id: u32,
        stream_handle_hint: u32,
        media_channel: u8,
        codec: crate::media::VideoCodec,
    ) -> u32 {
        let key = (media_channel, codec);
        // 1. Header routing preserves main/sub identity when both streams use
        //    the same channel and codec.
        if header_stream_id != 0 && self.stream_subs_by_id.contains_key(&header_stream_id) {
            self.pending_subscribe_ids
                .retain(|&id| id != header_stream_id);
            self.last_stream_metadata = None;
            return header_stream_id;
        }
        // 2. (channel, codec) mapping — most reliable when camera doesn't
        //    differentiate streams via hint or encryption_offset.  Works for
        //    multi-channel cameras (e.g. Duo 3) because channel is encoded
        //    in the media magic bytes.
        if let Some(&stream_id) = self.stream_id_by_channel_codec.get(&key) {
            // Post-hoc correction: if an InfoV1/V2 arrived since the last
            // video frame and its resolution contradicts the cached
            // assignment, swap the (ch,codec)→stream_id cache entries.
            // This handles cameras (e.g. Reolink RLC-811A) where InfoV1
            // arrives AFTER the first IFrame rather than before it.
            if let Some(info) = self.last_stream_metadata.take()
                && info.width > 0
                && info.height > 0
            {
                let expected_matches = self.stream_subs_by_id.get(&stream_id).is_none_or(|s| {
                    s.expected_width == 0
                        || s.expected_height == 0
                        || (s.expected_width == info.width && s.expected_height == info.height)
                });
                if !expected_matches
                    && let Some((&correct_id, _)) = self.stream_subs_by_id.iter().find(|(_, s)| {
                        s.expected_width == info.width && s.expected_height == info.height
                    })
                {
                    let other_key = self
                        .stream_id_by_channel_codec
                        .iter()
                        .find(|(k, v)| **k != key && **v == correct_id)
                        .map(|(k, _)| *k);
                    self.stream_id_by_channel_codec.insert(key, correct_id);
                    if let Some(ok) = other_key {
                        self.stream_id_by_channel_codec.insert(ok, stream_id);
                    }
                    tracing::info!(
                        old_sid = stream_id,
                        new_sid = correct_id,
                        info_w = info.width,
                        info_h = info.height,
                        "resolve: corrected stream assignment via InfoV1 resolution"
                    );
                    return correct_id;
                }
            }
            return stream_id;
        }
        tracing::info!(
            ?codec, ch = media_channel, hint = stream_handle_hint, header_sid = header_stream_id,
            handle_map = ?self.stream_id_by_remote_handle, pending = ?self.pending_subscribe_ids,
            last_info = ?self.last_stream_metadata,
            "resolve_video_stream_id: no cached (ch,codec) mapping",
        );
        // 3. Known remote handle mapping (from XML ack)
        // Ambiguous handles are already removed in maybe_update_stream_handle_mapping,
        // so any mapping that survives here is reliable.
        if let Some(&stream_id) = self.stream_id_by_remote_handle.get(&stream_handle_hint) {
            self.stream_id_by_channel_codec.insert(key, stream_id);
            return stream_id;
        }
        // 4. Hint matches a local subscription directly
        if stream_handle_hint != 0 && self.stream_subs_by_id.contains_key(&stream_handle_hint) {
            return stream_handle_hint;
        }
        // 5. Auto-learn: pop a pending subscribe ID, preferring one whose
        //    (a) channel matches the arriving frame, AND
        //    (b) expected resolution matches the last stream metadata header.
        //    This handles cameras where InfoV1 precedes the IFrame in the
        //    same BC body (the ideal case) or that don't echo msg_num.
        let learned = self
            .pop_pending_for_channel_and_resolution(media_channel)
            .or_else(|| self.pop_pending_for_channel(media_channel))
            .or_else(|| self.pending_subscribe_ids.pop_front());
        if let Some(stream_id) = learned {
            self.last_stream_metadata = None; // consumed
            self.stream_id_by_channel_codec.insert(key, stream_id);
            return stream_id;
        }
        // 6. Fallback to header stream_id
        if header_stream_id != 0 {
            return header_stream_id;
        }
        0
    }

    /// Pop a pending subscribe ID whose subscription channel matches
    /// `media_channel`.  Used to prefer channel-matched auto-learning
    /// on multi-channel cameras.
    fn pop_pending_for_channel(&mut self, media_channel: u8) -> Option<u32> {
        let pos = self.pending_subscribe_ids.iter().position(|&id| {
            self.stream_subs_by_id
                .get(&id)
                .is_some_and(|sub| sub.channel == media_channel)
        })?;
        self.pending_subscribe_ids.remove(pos)
    }

    /// Pop a pending subscribe ID matching both the media channel AND the
    /// last-seen stream metadata resolution. This is the primary disambiguation
    /// for cameras that share the same remote handle and encryption_offset
    /// for all streams.  The caller must have seen an InfoV1/V2 before the
    /// video frame for this to work (which is the normal camera behaviour).
    fn pop_pending_for_channel_and_resolution(&mut self, media_channel: u8) -> Option<u32> {
        let info = self.last_stream_metadata.as_ref()?;
        if info.width == 0 && info.height == 0 {
            return None;
        }
        let pos = self.pending_subscribe_ids.iter().position(|&id| {
            self.stream_subs_by_id.get(&id).is_some_and(|sub| {
                sub.channel == media_channel
                    && sub.expected_width > 0
                    && sub.expected_height > 0
                    && sub.expected_width == info.width
                    && sub.expected_height == info.height
            })
        })?;
        self.pending_subscribe_ids.remove(pos)
    }

    fn maybe_update_stream_handle_mapping(
        &mut self,
        body_start: usize,
        body_len: usize,
        payload_offset: Option<u32>,
    ) -> Result<(), BcError> {
        let full_body = &self.recv_buf[body_start..body_start + body_len];

        // The BC body may have two parts: Extension XML (0..payload_offset)
        // and Payload XML (payload_offset..body_len). The Preview ack
        // (channelId, handle, streamType) is in the payload.
        let (ext_data, payload_data) = payload_offset.map_or((&[] as &[u8], full_body), |offset| {
            let offset = offset as usize;
            if offset > 0 && offset < body_len {
                (&full_body[..offset], &full_body[offset..])
            } else {
                (&[] as &[u8], full_body)
            }
        });

        // Log raw data for diagnostics
        if !ext_data.is_empty() {
            if let Ok(ext_str) = core::str::from_utf8(ext_data) {
                tracing::debug!(ext = %ext_str, "stream ack: Extension XML");
            } else {
                tracing::debug!(ext_len = ext_data.len(), "stream ack: Extension binary");
            }
        }
        if let Ok(payload_str) = core::str::from_utf8(payload_data) {
            tracing::debug!(payload = %payload_str, "stream ack: Payload XML");
        } else {
            tracing::debug!(
                payload_len = payload_data.len(),
                "stream ack: Payload binary"
            );
        }

        // Try parsing the Payload portion first (contains <body><Preview> with
        // channelId, handle, streamType).  Fall back to full body if payload
        // parse yields nothing.
        let parsed = parse_stream_ack(payload_data).or_else(|| {
            if !ext_data.is_empty() {
                parse_stream_ack(full_body)
            } else {
                None
            }
        });

        let Some((channel, remote_handle, stream_type)) = parsed else {
            return Ok(());
        };

        let local_stream_id = stream_type
            .and_then(|st| {
                let sid = self
                    .stream_ids_by_key
                    .get(&stream_key(channel, st))
                    .copied()?;
                // Drain from pending since we positively identified via stream_type.
                self.pending_subscribe_ids.retain(|&id| id != sid);
                Some(sid)
            })
            .or_else(|| self.pending_subscribe_ids.pop_front());

        tracing::debug!(
            channel, remote_handle, ?stream_type, ?local_stream_id,
            pending = ?self.pending_subscribe_ids,
            handle_map = ?self.stream_id_by_remote_handle,
            "stream ack",
        );

        if let Some(stream_id) = local_stream_id {
            // Detect shared/ambiguous remote handles: if a different stream
            // was already mapped to this same remote_handle, the handle
            // can't disambiguate — remove the mapping entirely.
            if let Some(&existing) = self.stream_id_by_remote_handle.get(&remote_handle)
                && existing != stream_id
            {
                self.stream_id_by_remote_handle.remove(&remote_handle);
                self.remote_handle_by_stream_id.remove(&existing);
                return Ok(());
            }
            self.stream_id_by_remote_handle
                .insert(remote_handle, stream_id);
            self.remote_handle_by_stream_id
                .insert(stream_id, remote_handle);
        }

        Ok(())
    }

    fn queue_send(&mut self, header: &PacketHeader, body: &[u8]) -> Result<(), BcError> {
        let body_start = self.reserve_send(header, body.len())?;
        if !body.is_empty() {
            self.send_buf[body_start..body_start + body.len()].copy_from_slice(body);

            // Encrypt body for non-login messages using negotiated encryption.
            // Login messages handle their own encryption in dispatch_login.
            if header.msg_id != crate::COMMAND_LOGIN {
                let channel_id = (header.encryption_offset & 0xFF) as u8;
                self.encrypt_body(body_start, body.len(), channel_id, header.extension);
            }
        }

        Ok(())
    }

    fn queue_correlated_send(
        &mut self,
        header: PacketHeader,
        body: &[u8],
        kind: PendingCommandKind,
    ) -> Result<u16, BcError> {
        if self.pending_commands.len() >= MAX_PENDING_COMMANDS {
            return Err(BcError::Protocol("pending command table full"));
        }
        let msg_num = self.allocate_msg_num();
        let header = header.with_message_number(msg_num);
        self.pending_commands.insert((header.msg_id, msg_num), kind);
        if let Err(error) = self.queue_send(&header, body) {
            self.pending_commands.remove(&(header.msg_id, msg_num));
            return Err(error);
        }
        Ok(msg_num)
    }

    fn queue_correlated_send_parts(
        &mut self,
        header: PacketHeader,
        extension: &[u8],
        body: &[u8],
        binary_body: bool,
        kind: PendingCommandKind,
    ) -> Result<u16, BcError> {
        if self.pending_commands.len() >= MAX_PENDING_COMMANDS {
            return Err(BcError::Protocol("pending command table full"));
        }
        let msg_num = self.allocate_msg_num();
        let header = header.with_message_number(msg_num);
        self.pending_commands.insert((header.msg_id, msg_num), kind);
        if let Err(error) = self.queue_send_parts(&header, extension, body, binary_body) {
            self.pending_commands.remove(&(header.msg_id, msg_num));
            return Err(error);
        }
        Ok(msg_num)
    }

    fn queue_send_parts(
        &mut self,
        header: &PacketHeader,
        extension: &[u8],
        body: &[u8],
        binary_body: bool,
    ) -> Result<(), BcError> {
        let body_len = extension
            .len()
            .checked_add(body.len())
            .ok_or(BcError::Protocol("outgoing message length overflow"))?;
        if header.body_len
            != u32::try_from(body_len)
                .map_err(|_| BcError::Protocol("outgoing message exceeds protocol limit"))?
        {
            return Err(BcError::InvalidHeader("body length does not match header"));
        }
        let body_start = self.reserve_send(header, body_len)?;
        let payload_start = body_start + extension.len();
        self.send_buf[body_start..payload_start].copy_from_slice(extension);
        self.send_buf[payload_start..payload_start + body.len()].copy_from_slice(body);

        if header.msg_id != crate::COMMAND_LOGIN {
            let channel_id = (header.encryption_offset & 0xFF) as u8;
            if !extension.is_empty() {
                self.encrypt_body(body_start, extension.len(), channel_id, None);
            }
            if !binary_body && !body.is_empty() {
                self.encrypt_body(payload_start, body.len(), channel_id, None);
            }
        }

        Ok(())
    }

    fn reserve_send(&mut self, header: &PacketHeader, body_len: usize) -> Result<usize, BcError> {
        let mut hdr_buf = [0u8; HEADER_LEN_EXTENDED];
        let hdr_len = header.serialize(&mut hdr_buf);
        let total = hdr_len
            .checked_add(body_len)
            .ok_or(BcError::Protocol("outgoing message length overflow"))?;

        // Try to compact send buffer
        if self.send_end + total > self.send_buf.len() && self.send_start > 0 {
            let remaining = self.send_end - self.send_start;
            self.send_buf.copy_within(self.send_start..self.send_end, 0);
            self.send_start = 0;
            self.send_end = remaining;
        }

        if self.send_end + total > self.send_buf.len() {
            return Err(BcError::BufferTooSmall {
                needed: self.send_end + total,
                available: self.send_buf.len(),
            });
        }

        self.send_buf[self.send_end..self.send_end + hdr_len].copy_from_slice(&hdr_buf[..hdr_len]);
        self.send_end += hdr_len;
        let body_start = self.send_end;
        self.send_end += body_len;
        Ok(body_start)
    }

    /// Decrypt an incoming body region in recv_buf using the negotiated encryption.
    ///
    /// The body is split at `payload_offset`: Extension = `body[0..ext_len]`,
    /// Payload = `body[ext_len..]`.  Each part is decrypted with a fresh cipher
    /// state because each region starts with a fresh cipher state.
    fn decrypt_body(
        &mut self,
        body_start: usize,
        body_len: usize,
        channel_id: u8,
        payload_offset: Option<u32>,
    ) {
        let ext_len = payload_offset.unwrap_or(0).min(body_len as u32) as usize;
        let payload_start = body_start + ext_len;
        let payload_len = body_len.saturating_sub(ext_len);

        match self.encryption {
            EncryptionMode::BcEncrypt => {
                if ext_len > 0 {
                    crate::encryption::bc_xor(
                        &mut self.recv_buf[body_start..body_start + ext_len],
                        channel_id,
                    );
                }
                if payload_len > 0 {
                    crate::encryption::bc_xor(
                        &mut self.recv_buf[payload_start..payload_start + payload_len],
                        channel_id,
                    );
                }
            }
            EncryptionMode::Aes | EncryptionMode::FullAes => {
                if let Some(key) = self.aes_key {
                    if ext_len > 0 {
                        let cipher = crate::encryption::AesCipherState::new(key);
                        cipher.decrypt(&mut self.recv_buf[body_start..body_start + ext_len]);
                    }
                    if payload_len > 0 {
                        let cipher = crate::encryption::AesCipherState::new(key);
                        cipher.decrypt(
                            &mut self.recv_buf[payload_start..payload_start + payload_len],
                        );
                    }
                }
            }
            EncryptionMode::None => {}
        }
    }

    /// Encrypt an outgoing body region in send_buf using the negotiated encryption.
    ///
    /// Splits at `payload_offset` with fresh cipher state per part.
    fn encrypt_body(
        &mut self,
        body_start: usize,
        body_len: usize,
        channel_id: u8,
        payload_offset: Option<u32>,
    ) {
        let ext_len = payload_offset.unwrap_or(0).min(body_len as u32) as usize;
        let payload_start = body_start + ext_len;
        let payload_len = body_len.saturating_sub(ext_len);

        match self.encryption {
            EncryptionMode::BcEncrypt => {
                if ext_len > 0 {
                    crate::encryption::bc_xor(
                        &mut self.send_buf[body_start..body_start + ext_len],
                        channel_id,
                    );
                }
                if payload_len > 0 {
                    crate::encryption::bc_xor(
                        &mut self.send_buf[payload_start..payload_start + payload_len],
                        channel_id,
                    );
                }
            }
            EncryptionMode::Aes | EncryptionMode::FullAes => {
                if let Some(key) = self.aes_key {
                    if ext_len > 0 {
                        let cipher = crate::encryption::AesCipherState::new(key);
                        cipher.encrypt(&mut self.send_buf[body_start..body_start + ext_len]);
                    }
                    if payload_len > 0 {
                        let cipher = crate::encryption::AesCipherState::new(key);
                        cipher.encrypt(
                            &mut self.send_buf[payload_start..payload_start + payload_len],
                        );
                    }
                }
            }
            EncryptionMode::None => {}
        }
    }

    fn try_compact_recv(&mut self) {
        // Only compact when no pending events reference the buffer
        if self.recv_start > 0 && self.pending.is_empty() {
            let remaining = self.recv_end - self.recv_start;
            self.recv_buf.copy_within(self.recv_start..self.recv_end, 0);
            self.recv_start = 0;
            self.recv_end = remaining;
        }
    }

    fn take_pending_command(&mut self, header: PacketHeader) -> Option<PendingCommandKind> {
        let msg_num = header.message_number();
        (msg_num != 0)
            .then(|| self.pending_commands.remove(&(header.msg_id, msg_num)))
            .flatten()
    }

    fn push_command_failure(
        &mut self,
        header: PacketHeader,
        pending_command: Option<PendingCommandKind>,
    ) {
        if matches!(pending_command, Some(PendingCommandKind::Ping)) {
            self.pending_ping = None;
        }
        self.pending.push(PendingEvent::new(
            EventKind::CommandFailed {
                msg_id: header.msg_id,
                msg_num: header.message_number(),
                status: header.response_code(),
            },
            0,
            0,
        ));
    }

    fn reply_udp_keepalive(&mut self, request: PacketHeader) -> Result<(), BcError> {
        let header = PacketHeader {
            msg_id: crate::COMMAND_UDP_KEEP_ALIVE,
            body_len: 0,
            encryption_offset: request.encryption_offset,
            status_class: make_status(request.bc_class(), 0),
            extension: request.is_extended().then_some(0),
        };
        self.queue_send(&header, &[])
    }

    const fn is_connected(&self) -> bool {
        matches!(
            self.state,
            SessionState::Connected | SessionState::Authenticated
        )
    }

    fn check_keepalive(&mut self, now: Instant) -> Result<(), BcError> {
        if self.is_connected() && now.duration_since(self.last_send) >= self.keepalive_interval {
            if let Some(msg_num) = self.pending_ping.take() {
                self.pending_commands
                    .remove(&(crate::COMMAND_PING, msg_num));
                self.missed_pings = self.missed_pings.saturating_add(1);
                if self.missed_pings >= MAX_MISSED_PINGS && !self.pending.is_full() {
                    self.pending
                        .push(PendingEvent::new(EventKind::SessionTimeout, 0, 0));
                    return Ok(());
                }
            }
            self.send_ping()?;
            self.last_send = now;
        }
        Ok(())
    }

    fn check_relogin(&mut self, now: Instant) -> Result<(), BcError> {
        if self.state != SessionState::Connected {
            return Ok(());
        }
        if let (Some(params), Some(last)) = (self.login_params, self.last_login)
            && now.duration_since(last) >= self.relogin_interval
        {
            self.start_login(params)?;
        }
        Ok(())
    }

    fn check_stream_watchdog(&mut self, now: Instant) {
        if self.active_streams > 0
            && now.duration_since(self.last_recv) >= self.stream_watchdog_interval
            && !self.pending.is_full()
        {
            self.pending
                .push(PendingEvent::new(EventKind::SessionTimeout, 0, 0));
        }
    }

    fn next_deadline(&self) -> Instant {
        let keepalive_at = self.last_send + self.keepalive_interval;
        let mut deadline = keepalive_at;

        if self.active_streams > 0 {
            let watchdog_at = self.last_recv + self.stream_watchdog_interval;
            deadline = deadline.min(watchdog_at);
        }

        if let Some(last_login) = self.last_login
            && self.state == SessionState::Connected
        {
            let relogin_at = last_login + self.relogin_interval;
            deadline = deadline.min(relogin_at);
        }

        deadline
    }

    /// Set the session state (for testing and advanced use).
    pub const fn set_state(&mut self, state: SessionState) {
        self.state = state;
    }

    /// Set the active stream count (for testing and advanced use).
    pub const fn set_active_streams(&mut self, count: u8) {
        self.active_streams = count;
    }
}

const fn padding_len(payload_len: usize) -> usize {
    (8 - payload_len % 8) % 8
}

/// Locate a media frame magic at the start of a `COMMAND_STREAM` body.
///
/// Media frames are 8-byte aligned, so the trailing padding of one frame can spill
/// into the next message and push the following frame's magic up to seven bytes in.
/// Such a body is still media: it must not be decrypted, and it must not be treated
/// as continuation data for a frame that is still accumulating.
///
/// Only leading zero bytes are skipped, so raw video continuation data cannot be
/// misread as a frame start unless it literally contains a magic value.
fn media_frame_offset(body: &[u8]) -> Option<(usize, crate::media::MediaMagic)> {
    for offset in 0..MEDIA_FRAME_ALIGNMENT {
        if offset + 4 > body.len() {
            return None;
        }
        if offset > 0 && body[offset - 1] != 0 {
            return None;
        }
        let magic = u32::from_le_bytes([
            body[offset],
            body[offset + 1],
            body[offset + 2],
            body[offset + 3],
        ]);
        if let Some(magic) = crate::media::MediaMagic::from_u32(magic) {
            return Some((offset, magic));
        }
    }
    None
}

fn find_media_magic(data: &[u8]) -> Option<usize> {
    (1..data.len().saturating_sub(3)).find(|&offset| {
        let magic = u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]);
        crate::media::MediaMagic::from_u32(magic).is_some()
    })
}

/// Scan for the Baichuan magic bytes in a data slice.
/// Returns the offset of the first occurrence after position 0, or None.
fn scan_for_magic(data: &[u8]) -> Option<usize> {
    (1..data.len().saturating_sub(3)).find(|&i| has_header_magic(&data[i..]))
}

const fn stream_key(channel: u8, stream_type: crate::stream::StreamType) -> u16 {
    ((channel as u16) << 8) | stream_type_wire_id(stream_type) as u16
}

const fn stream_type_wire_id(stream_type: crate::stream::StreamType) -> u8 {
    match stream_type {
        crate::stream::StreamType::Main => 0,
        crate::stream::StreamType::Sub => 1,
        crate::stream::StreamType::Extern => 2,
    }
}

fn parse_stream_ack(data: &[u8]) -> Option<(u8, u32, Option<crate::stream::StreamType>)> {
    let mut channel: Option<u8> = None;
    let mut handle: Option<u32> = None;
    let mut stream_type: Option<crate::stream::StreamType> = None;

    // Ignore XML parse errors — some cameras include binary Extension data
    // that breaks the parser, but we may still extract useful fields before
    // the error.
    let _ = crate::xml::parse_xml(data, |name, text| match name {
        "channelId" => {
            if let Ok(v) = text.parse::<u8>() {
                channel = Some(v);
            }
        }
        "handle" => {
            if let Ok(v) = text.parse::<u32>() {
                handle = Some(v);
            }
        }
        "streamType" => {
            stream_type = match text {
                "mainStream" => Some(crate::stream::StreamType::Main),
                "subStream" => Some(crate::stream::StreamType::Sub),
                "externStream" => Some(crate::stream::StreamType::Extern),
                _ => None,
            };
        }
        _ => {}
    });

    channel.zip(handle).map(|(c, h)| (c, h, stream_type))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_wire_message(msg_id: u32, body: &[u8]) -> Vec<u8> {
        let header = PacketHeader {
            msg_id,
            body_len: body.len() as u32,
            encryption_offset: body.len() as u32,
            status_class: make_status(BC_CLASS_MODERN_SHORT, 0),
            extension: None,
        };
        let mut hdr_buf = [0u8; HEADER_LEN_EXTENDED];
        let hdr_len = header.serialize(&mut hdr_buf);
        let mut wire = Vec::new();
        wire.extend_from_slice(&hdr_buf[..hdr_len]);
        wire.extend_from_slice(body);
        wire
    }

    fn video_body(magic: u32, codec: &[u8; 4], data: &[u8], data_len: u32) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&magic.to_le_bytes());
        body.extend_from_slice(codec);
        body.extend_from_slice(&data_len.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&1_000u32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(data);
        body
    }

    fn video_body_with_header(
        magic: u32,
        codec: &[u8; 4],
        additional_header: &[u8],
        data: &[u8],
    ) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&magic.to_le_bytes());
        body.extend_from_slice(codec);
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(&(additional_header.len() as u32).to_le_bytes());
        body.extend_from_slice(&1_000u32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(additional_header);
        body.extend_from_slice(data);
        body.resize(body.len() + padding_len(data.len()), 0);
        body
    }

    #[test]
    fn client_starts_disconnected() {
        let now = Instant::now();
        let s = BcSession::default_client(now);
        assert_eq!(s.role(), Role::Client);
        assert_eq!(s.state(), SessionState::Disconnected);
    }

    #[test]
    fn camera_starts_awaiting_login() {
        let now = Instant::now();
        let s = BcSession::default_camera(now);
        assert_eq!(s.role(), Role::Camera);
        assert_eq!(s.state(), SessionState::AwaitingLogin);
    }

    #[test]
    fn interleaved_chunked_video_frames_keep_separate_accumulators() {
        let now = Instant::now();
        let mut session = BcSession::default_client(now);
        let main_start = video_body(crate::media::MEDIA_MAGIC_IFRAME_BASE, b"H265", b"main", 8);
        let sub_start = video_body(crate::media::MEDIA_MAGIC_IFRAME_BASE, b"H264", b"sub-", 8);

        session.recv_buf[..main_start.len()].copy_from_slice(&main_start);
        session
            .dispatch_stream(0, main_start.len(), 1, None)
            .unwrap();

        let sub_offset = 64;
        session.recv_buf[sub_offset..sub_offset + sub_start.len()].copy_from_slice(&sub_start);
        session
            .dispatch_stream(sub_offset, sub_start.len(), 2, None)
            .unwrap();

        let main_tail_offset = 128;
        session.recv_buf[main_tail_offset..main_tail_offset + 4].copy_from_slice(b"tail");
        session
            .dispatch_stream(main_tail_offset, 4, 1, None)
            .unwrap();

        let sub_tail_offset = 132;
        session.recv_buf[sub_tail_offset..sub_tail_offset + 4].copy_from_slice(b"tail");
        session
            .dispatch_stream(sub_tail_offset, 4, 2, None)
            .unwrap();

        let mut output = [0u8; 32];
        match session.poll_output(&mut output).unwrap() {
            Output::Event(Event::VideoFrame {
                stream_id, data, ..
            }) => {
                assert_eq!(stream_id, 1);
                assert_eq!(data, b"maintail");
            }
            other => panic!("expected main video frame, got {other:?}"),
        }
        match session.poll_output(&mut output).unwrap() {
            Output::Event(Event::VideoFrame {
                stream_id, data, ..
            }) => {
                assert_eq!(stream_id, 2);
                assert_eq!(data, b"sub-tail");
            }
            other => panic!("expected sub video frame, got {other:?}"),
        }
    }

    #[test]
    fn media_frame_offset_finds_frame_behind_alignment_padding() {
        let frame = video_body(crate::media::MEDIA_MAGIC_IFRAME_BASE, b"H265", b"abc", 3);
        for padding in 0..MEDIA_FRAME_ALIGNMENT {
            let mut body = vec![0u8; padding];
            body.extend_from_slice(&frame);
            let (offset, _) = media_frame_offset(&body).expect("padded frame must be recognised");
            assert_eq!(offset, padding);
        }
    }

    #[test]
    fn media_frame_offset_ignores_video_continuation_bytes() {
        assert!(media_frame_offset(&[0, 0, 0, 1, 0x26, 0x01, 0xaa, 0xbb]).is_none());

        let mut non_padding_prefix = vec![0x41u8; 4];
        non_padding_prefix.extend_from_slice(&crate::media::MEDIA_MAGIC_IFRAME_BASE.to_le_bytes());
        assert!(media_frame_offset(&non_padding_prefix).is_none());
    }

    #[test]
    fn padded_frame_is_not_appended_to_active_accumulation() {
        let now = Instant::now();
        let mut session = BcSession::default_client(now);

        let partial = video_body(crate::media::MEDIA_MAGIC_IFRAME_BASE, b"H265", b"abc", 5);
        session.recv_buf[..partial.len()].copy_from_slice(&partial);
        session.dispatch_stream(0, partial.len(), 1, None).unwrap();

        // The previous frame's alignment padding spills in front of the next frame.
        let complete = video_body(crate::media::MEDIA_MAGIC_PFRAME_BASE, b"H265", b"sub!", 4);
        let mut padded = vec![0u8; 3];
        padded.extend_from_slice(&complete);
        let padded_offset = 128;
        session.recv_buf[padded_offset..padded_offset + padded.len()].copy_from_slice(&padded);
        session
            .dispatch_stream(padded_offset, padded.len(), 2, None)
            .unwrap();

        let tail_offset = 256;
        session.recv_buf[tail_offset..tail_offset + 2].copy_from_slice(b"de");
        session.dispatch_stream(tail_offset, 2, 1, None).unwrap();

        let mut output = [0u8; 32];
        match session.poll_output(&mut output).unwrap() {
            Output::Event(Event::VideoFrame { data, .. }) => assert_eq!(data, b"sub!"),
            other => panic!("expected padded frame to be emitted, got {other:?}"),
        }
        match session.poll_output(&mut output).unwrap() {
            Output::Event(Event::VideoFrame { data, .. }) => assert_eq!(data, b"abcde"),
            other => panic!("expected accumulation to stay intact, got {other:?}"),
        }
    }

    fn aac_body(data: &[u8], declared_len: u16) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&crate::media::MEDIA_MAGIC_AAC.to_le_bytes());
        body.extend_from_slice(&declared_len.to_le_bytes());
        body.extend_from_slice(&declared_len.to_le_bytes());
        body.extend_from_slice(data);
        body
    }

    #[test]
    fn split_audio_frame_is_reassembled_across_messages() {
        let now = Instant::now();
        let mut session = BcSession::default_client(now);

        let head = aac_body(b"aac", 5);
        session.recv_buf[..head.len()].copy_from_slice(&head);
        session.dispatch_stream(0, head.len(), 1, None).unwrap();
        assert_eq!(session.stats().audio_accum_started, 1);

        let tail_offset = 64;
        session.recv_buf[tail_offset..tail_offset + 2].copy_from_slice(b"io");
        session.dispatch_stream(tail_offset, 2, 1, None).unwrap();

        let mut output = [0u8; 32];
        match session.poll_output(&mut output).unwrap() {
            Output::Event(Event::AudioFrame {
                stream_id, data, ..
            }) => {
                assert_eq!(stream_id, 1);
                assert_eq!(data, b"aacio");
            }
            other => panic!("expected reassembled audio frame, got {other:?}"),
        }
        assert_eq!(session.stats().audio_accum_completed, 1);
    }

    #[test]
    fn video_after_split_audio_frame_is_still_parsed() {
        let now = Instant::now();
        let mut session = BcSession::default_client(now);

        // Mirrors the observed wire pattern: a short message carries an audio
        // frame whose declared length overruns the message body.
        let head = aac_body(b"aac", 5);
        session.recv_buf[..head.len()].copy_from_slice(&head);
        session.dispatch_stream(0, head.len(), 1, None).unwrap();

        // The next message opens with the audio tail plus its alignment padding,
        // then continues with a complete video frame.
        let frame = video_body(crate::media::MEDIA_MAGIC_IFRAME_BASE, b"H265", b"vid!", 4);
        let mut next = Vec::from(&b"io"[..]);
        let audio_total = 8 + 5;
        next.resize(2 + (crate::media::align8(audio_total) - audio_total), 0);
        next.extend_from_slice(&frame);

        let offset = 64;
        session.recv_buf[offset..offset + next.len()].copy_from_slice(&next);
        session
            .dispatch_stream(offset, next.len(), 1, None)
            .unwrap();

        assert_eq!(session.stats().stream_bodies_unrecognized, 0);

        let mut output = [0u8; 32];
        match session.poll_output(&mut output).unwrap() {
            Output::Event(Event::AudioFrame { data, .. }) => assert_eq!(data, b"aacio"),
            other => panic!("expected reassembled audio frame, got {other:?}"),
        }
        match session.poll_output(&mut output).unwrap() {
            Output::Event(Event::VideoFrame { data, .. }) => assert_eq!(data, b"vid!"),
            other => panic!("expected video frame after split audio, got {other:?}"),
        }
    }

    #[test]
    fn split_audio_frame_spanning_three_messages_is_reassembled() {
        let now = Instant::now();
        let mut session = BcSession::default_client(now);

        let head = aac_body(b"a", 5);
        session.recv_buf[..head.len()].copy_from_slice(&head);
        session.dispatch_stream(0, head.len(), 1, None).unwrap();

        session.recv_buf[64..66].copy_from_slice(b"ac");
        session.dispatch_stream(64, 2, 1, None).unwrap();

        session.recv_buf[128..130].copy_from_slice(b"io");
        session.dispatch_stream(128, 2, 1, None).unwrap();

        let mut output = [0u8; 32];
        match session.poll_output(&mut output).unwrap() {
            Output::Event(Event::AudioFrame { data, .. }) => assert_eq!(data, b"aacio"),
            other => panic!("expected reassembled audio frame, got {other:?}"),
        }
    }

    fn video_body_with_ext(magic: u32, codec: &[u8], payload: &[u8], ext: usize) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&magic.to_le_bytes());
        body.extend_from_slice(codec);
        body.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        body.extend_from_slice(&(ext as u32).to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&vec![0xAAu8; ext]);
        body.extend_from_slice(payload);
        body
    }

    #[test]
    fn video_header_split_across_messages_is_completed() {
        let now = Instant::now();
        let mut session = BcSession::default_client(now);

        let frame = video_body_with_ext(
            crate::media::MEDIA_MAGIC_IFRAME_BASE,
            b"H265",
            b"payload!",
            80,
        );
        let header_total = 24 + 80;
        let split = 40;

        session.recv_buf[..split].copy_from_slice(&frame[..split]);
        session.dispatch_stream(0, split, 1, None).unwrap();
        assert_eq!(session.stats().split_headers, 1);
        assert!(session.video_accums.is_empty());

        let rest = &frame[split..];
        let offset = 256;
        session.recv_buf[offset..offset + rest.len()].copy_from_slice(rest);
        session
            .dispatch_stream(offset, rest.len(), 1, None)
            .unwrap();

        assert_eq!(session.stats().stream_bodies_unrecognized, 0);
        let mut output = [0u8; 64];
        match session.poll_output(&mut output).unwrap() {
            Output::Event(Event::VideoFrame {
                data, is_keyframe, ..
            }) => {
                assert!(is_keyframe);
                assert_eq!(data, b"payload!");
            }
            other => panic!("expected frame with split header, got {other:?}"),
        }
        assert_eq!(header_total, 104);
    }

    #[test]
    fn video_header_split_before_extension_keeps_payload_intact() {
        let now = Instant::now();
        let mut session = BcSession::default_client(now);

        // Split at exactly 24 bytes: parse_video_header succeeds but the
        // extension header still lies in the next message.
        let frame =
            video_body_with_ext(crate::media::MEDIA_MAGIC_PFRAME_BASE, b"H265", b"abcd", 112);
        session.recv_buf[..24].copy_from_slice(&frame[..24]);
        session.dispatch_stream(0, 24, 1, None).unwrap();
        assert_eq!(session.stats().split_headers, 1);

        let rest = &frame[24..];
        let offset = 512;
        session.recv_buf[offset..offset + rest.len()].copy_from_slice(rest);
        session
            .dispatch_stream(offset, rest.len(), 1, None)
            .unwrap();

        let mut output = [0u8; 64];
        match session.poll_output(&mut output).unwrap() {
            Output::Event(Event::VideoFrame { data, .. }) => assert_eq!(data, b"abcd"),
            other => panic!("expected intact payload, got {other:?}"),
        }
    }

    fn push_video_frame(out: &mut Vec<u8>, magic: u32, payload: &[u8], ext: usize) {
        out.extend_from_slice(&magic.to_le_bytes());
        out.extend_from_slice(b"H265");
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&(ext as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend(std::iter::repeat_n(0xAAu8, ext));
        out.extend_from_slice(payload);
        out.extend(std::iter::repeat_n(0u8, padding_len(payload.len())));
    }

    fn push_aac_frame(out: &mut Vec<u8>, payload: &[u8]) {
        out.extend_from_slice(&crate::media::MEDIA_MAGIC_AAC.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        out.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        out.extend_from_slice(payload);
        let total = 8 + payload.len();
        out.extend(std::iter::repeat_n(
            0u8,
            crate::media::align8(total) - total,
        ));
    }

    /// A media stream exercising both header sizes seen on real cameras, mixed
    /// video and audio, and payloads that need alignment padding.
    fn chunking_fixture() -> (Vec<u8>, Vec<Vec<u8>>) {
        let payloads: Vec<Vec<u8>> = vec![
            b"keyframe-payload-0123456789".to_vec(),
            b"aac-audio-frame".to_vec(),
            b"pframe-a".to_vec(),
            b"aac2".to_vec(),
            b"pframe-b-with-longer-payload".to_vec(),
        ];
        let mut stream = Vec::new();
        push_video_frame(
            &mut stream,
            crate::media::MEDIA_MAGIC_IFRAME_BASE,
            &payloads[0],
            80,
        );
        push_aac_frame(&mut stream, &payloads[1]);
        push_video_frame(
            &mut stream,
            crate::media::MEDIA_MAGIC_PFRAME_BASE,
            &payloads[2],
            112,
        );
        push_aac_frame(&mut stream, &payloads[3]);
        push_video_frame(
            &mut stream,
            crate::media::MEDIA_MAGIC_PFRAME_BASE,
            &payloads[4],
            0,
        );
        (stream, payloads)
    }

    fn frames_for_chunk_size(stream: &[u8], chunk: usize) -> Vec<Vec<u8>> {
        let now = Instant::now();
        let mut session = BcSession::default_client(now);
        let mut frames = Vec::new();
        let mut buf = [0u8; 256];

        for piece in stream.chunks(chunk) {
            session.recv_buf[..piece.len()].copy_from_slice(piece);
            session.dispatch_stream(0, piece.len(), 1, None).unwrap();
            loop {
                match session.poll_output(&mut buf) {
                    Ok(Output::Event(Event::VideoFrame { data, .. }))
                    | Ok(Output::Event(Event::AudioFrame { data, .. })) => {
                        frames.push(data.to_vec());
                    }
                    Ok(Output::Event(_)) => {}
                    _ => break,
                }
            }
        }
        frames
    }

    #[test]
    fn frame_output_is_independent_of_message_chunking() {
        let (stream, expected) = chunking_fixture();

        // Message bodies are always a multiple of the 8-byte frame alignment,
        // so every such split must reproduce the same frames.
        for chunk in (8..=stream.len()).step_by(8) {
            let frames = frames_for_chunk_size(&stream, chunk);
            assert_eq!(
                frames, expected,
                "chunk size {chunk} changed the decoded frames"
            );
        }
    }

    #[test]
    fn continuation_padding_does_not_hide_following_frame() {
        let now = Instant::now();
        let mut session = BcSession::default_client(now);
        let first_start = video_body(crate::media::MEDIA_MAGIC_IFRAME_BASE, b"H265", b"abc", 5);
        session.recv_buf[..first_start.len()].copy_from_slice(&first_start);
        session
            .dispatch_stream(0, first_start.len(), 1, None)
            .unwrap();

        let second = video_body(crate::media::MEDIA_MAGIC_PFRAME_BASE, b"H265", b"next", 4);
        let mut continuation = Vec::from(&b"de\0\0\0"[..]);
        continuation.extend_from_slice(&second);
        let offset = 64;
        session.recv_buf[offset..offset + continuation.len()].copy_from_slice(&continuation);
        session
            .dispatch_stream(offset, continuation.len(), 1, None)
            .unwrap();

        let mut output = [0u8; 32];
        match session.poll_output(&mut output).unwrap() {
            Output::Event(Event::VideoFrame { data, .. }) => assert_eq!(data, b"abcde"),
            other => panic!("expected completed video frame, got {other:?}"),
        }
        match session.poll_output(&mut output).unwrap() {
            Output::Event(Event::VideoFrame {
                is_keyframe, data, ..
            }) => {
                assert!(!is_keyframe);
                assert_eq!(data, b"next");
            }
            other => panic!("expected following video frame, got {other:?}"),
        }
    }

    #[test]
    fn padding_in_next_message_does_not_hide_following_frame() {
        let now = Instant::now();
        let mut session = BcSession::default_client(now);
        let first_start = video_body(crate::media::MEDIA_MAGIC_IFRAME_BASE, b"H265", b"abc", 5);
        session.recv_buf[..first_start.len()].copy_from_slice(&first_start);
        session
            .dispatch_stream(0, first_start.len(), 1, None)
            .unwrap();

        let tail_offset = 64;
        session.recv_buf[tail_offset..tail_offset + 2].copy_from_slice(b"de");
        session.dispatch_stream(tail_offset, 2, 1, None).unwrap();

        let second = video_body(crate::media::MEDIA_MAGIC_PFRAME_BASE, b"H265", b"next", 4);
        let mut padded_second = Vec::from(&b"\0\0\0"[..]);
        padded_second.extend_from_slice(&second);
        let second_offset = 96;
        session.recv_buf[second_offset..second_offset + padded_second.len()]
            .copy_from_slice(&padded_second);
        session
            .dispatch_stream(second_offset, padded_second.len(), 1, None)
            .unwrap();

        let mut output = [0u8; 32];
        match session.poll_output(&mut output).unwrap() {
            Output::Event(Event::VideoFrame { data, .. }) => assert_eq!(data, b"abcde"),
            other => panic!("expected completed video frame, got {other:?}"),
        }
        match session.poll_output(&mut output).unwrap() {
            Output::Event(Event::VideoFrame { data, .. }) => assert_eq!(data, b"next"),
            other => panic!("expected frame after split padding, got {other:?}"),
        }
    }

    #[test]
    fn video_padding_depends_on_payload_not_optional_header() {
        let now = Instant::now();
        let mut session = BcSession::default_client(now);
        let mut body = video_body_with_header(
            crate::media::MEDIA_MAGIC_IFRAME_BASE,
            b"H265",
            &[1, 2, 3, 4],
            b"first",
        );
        body.extend_from_slice(&video_body_with_header(
            crate::media::MEDIA_MAGIC_PFRAME_BASE,
            b"H265",
            &[],
            b"second",
        ));
        session.recv_buf[..body.len()].copy_from_slice(&body);
        session.dispatch_stream(0, body.len(), 1, None).unwrap();

        let mut output = [0u8; 32];
        match session.poll_output(&mut output).unwrap() {
            Output::Event(Event::VideoFrame { data, .. }) => assert_eq!(data, b"first"),
            other => panic!("expected first video frame, got {other:?}"),
        }
        match session.poll_output(&mut output).unwrap() {
            Output::Event(Event::VideoFrame { data, .. }) => assert_eq!(data, b"second"),
            other => panic!("expected second video frame, got {other:?}"),
        }
    }

    #[test]
    fn ping_command_produces_tcp_send() {
        let now = Instant::now();
        let mut s = BcSession::default_client(now);
        s.handle_input(Input::Command(Command::Ping)).unwrap();

        let mut buf = [0u8; 256];
        match s.poll_output(&mut buf).unwrap() {
            Output::TcpSend { data } => {
                // Should be a valid Baichuan header with msg_id = PING
                assert!(data.len() >= HEADER_LEN_SHORT);
                let (header, _) = PacketHeader::parse(data).unwrap();
                assert_eq!(header.msg_id, crate::COMMAND_PING);
                assert_eq!(header.body_len, 0);
            }
            other => panic!("expected TcpSend, got {other:?}"),
        }

        // Next poll should be Timeout
        match s.poll_output(&mut buf).unwrap() {
            Output::Timeout(_) => {}
            other => panic!("expected Timeout, got {other:?}"),
        }
    }

    #[test]
    fn complete_message_produces_event() {
        let now = Instant::now();
        let mut s = BcSession::default_client(now);
        let body = b"hello world";
        let wire = make_wire_message(9042, body);

        s.handle_input(Input::TcpData(now, &wire)).unwrap();

        let mut buf = [0u8; 256];
        match s.poll_output(&mut buf).unwrap() {
            Output::Event(Event::UnhandledMessage { msg_id, body: b }) => {
                assert_eq!(msg_id, 9042);
                assert_eq!(b, b"hello world");
            }
            other => panic!("expected UnhandledMessage, got {other:?}"),
        }
    }

    #[test]
    fn ping_response_produces_pong_event() {
        let now = Instant::now();
        let mut s = BcSession::default_client(now);
        s.handle_input(Input::Command(Command::Ping)).unwrap();
        let mut sent = [0u8; 256];
        let request = match s.poll_output(&mut sent).unwrap() {
            Output::TcpSend { data } => PacketHeader::parse(data).unwrap().0,
            other => panic!("expected ping TcpSend, got {other:?}"),
        };
        let response = PacketHeader {
            msg_id: crate::COMMAND_PING,
            body_len: 0,
            encryption_offset: request.encryption_offset,
            status_class: make_status(BC_CLASS_MODERN_EXT, 200),
            extension: Some(0),
        };
        let mut header_buf = [0u8; HEADER_LEN_EXTENDED];
        let wire = response.serialize(&mut header_buf);

        s.handle_input(Input::TcpData(now, &header_buf[..wire]))
            .unwrap();

        let mut buf = [0u8; 256];
        match s.poll_output(&mut buf).unwrap() {
            Output::Event(Event::Pong) => {}
            other => panic!("expected Pong, got {other:?}"),
        }
    }

    #[test]
    fn partial_message_waits_for_more_data() {
        let now = Instant::now();
        let mut s = BcSession::default_client(now);
        let wire = make_wire_message(9042, b"payload");

        // Feed only first 10 bytes
        s.handle_input(Input::TcpData(now, &wire[..10])).unwrap();

        let mut buf = [0u8; 256];
        match s.poll_output(&mut buf).unwrap() {
            Output::Timeout(_) => {} // no event yet
            other => panic!("expected Timeout, got {other:?}"),
        }

        // Feed remaining bytes
        s.handle_input(Input::TcpData(now, &wire[10..])).unwrap();

        match s.poll_output(&mut buf).unwrap() {
            Output::Event(Event::UnhandledMessage { msg_id, body }) => {
                assert_eq!(msg_id, 9042);
                assert_eq!(body, b"payload");
            }
            other => panic!("expected UnhandledMessage, got {other:?}"),
        }
    }

    #[test]
    fn multiple_messages_in_one_tcp_data() {
        let now = Instant::now();
        let mut s = BcSession::default_client(now);

        let mut wire = make_wire_message(9010, b"first");
        wire.extend(make_wire_message(9020, b"second"));

        s.handle_input(Input::TcpData(now, &wire)).unwrap();

        let mut buf = [0u8; 256];
        match s.poll_output(&mut buf).unwrap() {
            Output::Event(Event::UnhandledMessage { msg_id, body }) => {
                assert_eq!(msg_id, 9010);
                assert_eq!(body, b"first");
            }
            other => panic!("expected first message, got {other:?}"),
        }

        match s.poll_output(&mut buf).unwrap() {
            Output::Event(Event::UnhandledMessage { msg_id, body }) => {
                assert_eq!(msg_id, 9020);
                assert_eq!(body, b"second");
            }
            other => panic!("expected second message, got {other:?}"),
        }

        match s.poll_output(&mut buf).unwrap() {
            Output::Timeout(_) => {}
            other => panic!("expected Timeout, got {other:?}"),
        }
    }

    #[test]
    fn sends_drain_before_events() {
        let now = Instant::now();
        let mut s = BcSession::default_client(now);

        // Queue a send (ping)
        s.handle_input(Input::Command(Command::Ping)).unwrap();
        // Feed a message
        let wire = make_wire_message(99, b"data");
        s.handle_input(Input::TcpData(now, &wire)).unwrap();

        let mut buf = [0u8; 256];
        // First poll should be the TcpSend (sends drain first)
        match s.poll_output(&mut buf).unwrap() {
            Output::TcpSend { data } => {
                let (h, _) = PacketHeader::parse(data).unwrap();
                assert_eq!(h.msg_id, crate::COMMAND_PING);
            }
            other => panic!("expected TcpSend, got {other:?}"),
        }
        // Then the event
        match s.poll_output(&mut buf).unwrap() {
            Output::Event(Event::UnhandledMessage { msg_id, .. }) => {
                assert_eq!(msg_id, 99);
            }
            other => panic!("expected event, got {other:?}"),
        }
    }

    #[test]
    fn keepalive_fires_when_connected() {
        let now = Instant::now();
        let mut s = BcSession::new(
            BcSessionConfig {
                keepalive_interval: Duration::from_secs(5),
                ..BcSessionConfig::default_client()
            },
            now,
        );
        // Force to Connected state
        s.set_state(SessionState::Connected);

        // Advance time past keepalive interval
        let later = now + Duration::from_secs(6);
        s.handle_input(Input::Timeout(later)).unwrap();

        let mut buf = [0u8; 256];
        match s.poll_output(&mut buf).unwrap() {
            Output::TcpSend { data } => {
                let (h, _) = PacketHeader::parse(data).unwrap();
                assert_eq!(h.msg_id, crate::COMMAND_PING);
            }
            other => panic!("expected TcpSend (ping), got {other:?}"),
        }
    }

    #[test]
    fn keepalive_does_not_fire_when_disconnected() {
        let now = Instant::now();
        let mut s = BcSession::new(
            BcSessionConfig {
                keepalive_interval: Duration::from_secs(5),
                ..BcSessionConfig::default_client()
            },
            now,
        );
        assert_eq!(s.state(), SessionState::Disconnected);

        let later = now + Duration::from_secs(60);
        s.handle_input(Input::Timeout(later)).unwrap();

        let mut buf = [0u8; 256];
        match s.poll_output(&mut buf).unwrap() {
            Output::Timeout(_) => {} // no ping sent
            other => panic!("expected Timeout (no ping), got {other:?}"),
        }
    }

    #[test]
    fn stream_watchdog_fires_on_silence() {
        let now = Instant::now();
        let mut s = BcSession::new(
            BcSessionConfig {
                stream_watchdog_interval: Duration::from_secs(10),
                ..BcSessionConfig::default_client()
            },
            now,
        );
        s.set_state(SessionState::Connected);
        s.set_active_streams(1);

        let later = now + Duration::from_secs(11);
        s.handle_input(Input::Timeout(later)).unwrap();

        let mut buf = [0u8; 256];
        match s.poll_output(&mut buf).unwrap() {
            Output::TcpSend { .. } => {
                // Keepalive ping might fire first (also past interval)
                // Drain it, then check for SessionTimeout
            }
            Output::Event(Event::SessionTimeout) => return, // pass
            other => panic!("expected ping or SessionTimeout, got {other:?}"),
        }

        // Might need to drain the ping first, then get the event
        match s.poll_output(&mut buf).unwrap() {
            Output::Event(Event::SessionTimeout) => {} // pass
            other => panic!("expected SessionTimeout, got {other:?}"),
        }
    }

    #[test]
    fn stream_watchdog_does_not_fire_without_streams() {
        let now = Instant::now();
        let mut s = BcSession::new(
            BcSessionConfig {
                stream_watchdog_interval: Duration::from_secs(10),
                keepalive_interval: Duration::from_secs(999), // disable keepalive
                ..BcSessionConfig::default_client()
            },
            now,
        );
        s.set_state(SessionState::Connected);
        // active_streams = 0 (default)

        let later = now + Duration::from_secs(60);
        s.handle_input(Input::Timeout(later)).unwrap();

        let mut buf = [0u8; 256];
        match s.poll_output(&mut buf).unwrap() {
            Output::Timeout(_) => {} // no watchdog fired
            other => panic!("expected Timeout, got {other:?}"),
        }
    }

    #[test]
    fn timeout_deadline_reflects_keepalive() {
        let now = Instant::now();
        let mut s = BcSession::new(
            BcSessionConfig {
                keepalive_interval: Duration::from_secs(30),
                ..BcSessionConfig::default_client()
            },
            now,
        );

        let mut buf = [0u8; 64];
        match s.poll_output(&mut buf).unwrap() {
            Output::Timeout(deadline) => {
                assert_eq!(deadline, now + Duration::from_secs(30));
            }
            other => panic!("expected Timeout, got {other:?}"),
        }
    }

    #[test]
    fn garbage_before_message_is_skipped() {
        let now = Instant::now();
        let mut s = BcSession::default_client(now);

        let mut data = vec![0xFF, 0xFE, 0xFD, 0xFC, 0xFB];
        data.extend(make_wire_message(9077, b"ok"));

        s.handle_input(Input::TcpData(now, &data)).unwrap();

        let mut buf = [0u8; 256];
        match s.poll_output(&mut buf).unwrap() {
            Output::Event(Event::UnhandledMessage { msg_id, body }) => {
                assert_eq!(msg_id, 9077);
                assert_eq!(body, b"ok");
            }
            other => panic!("expected event, got {other:?}"),
        }
    }

    #[test]
    fn zero_length_body_message() {
        let now = Instant::now();
        let mut s = BcSession::default_client(now);
        let wire = make_wire_message(9002, &[]);

        s.handle_input(Input::TcpData(now, &wire)).unwrap();

        let mut buf = [0u8; 256];
        match s.poll_output(&mut buf).unwrap() {
            Output::Event(Event::UnhandledMessage { msg_id, body }) => {
                assert_eq!(msg_id, 9002);
                assert!(body.is_empty());
            }
            other => panic!("expected event, got {other:?}"),
        }
    }

    #[test]
    fn recv_buffer_compacts_after_events_drained() {
        let now = Instant::now();
        let mut s = BcSession::new(
            BcSessionConfig {
                tcp_recv_buf_size: 256, // small buffer to force compaction
                ..BcSessionConfig::default_client()
            },
            now,
        );

        let wire = make_wire_message(1, b"data");
        let msg_len = wire.len();

        // Fill and drain multiple times to verify compaction works
        for _ in 0..10 {
            s.handle_input(Input::TcpData(now, &wire)).unwrap();
            let mut buf = [0u8; 256];
            match s.poll_output(&mut buf).unwrap() {
                Output::Event(Event::UnhandledMessage { msg_id, body }) => {
                    assert_eq!(msg_id, 1);
                    assert_eq!(body, b"data");
                }
                other => panic!("expected event, got {other:?}"),
            }
            // After draining, buffer should have compacted
            // (recv_start should be 0 or small, not accumulated)
        }

        // If compaction wasn't working, the 256-byte buffer would overflow
        // after a few iterations (each message is ~24 bytes)
        assert!(msg_len < 256); // sanity check
    }

    #[test]
    fn multiple_pings_queued() {
        let now = Instant::now();
        let mut s = BcSession::default_client(now);

        s.handle_input(Input::Command(Command::Ping)).unwrap();
        s.handle_input(Input::Command(Command::Ping)).unwrap();

        let mut buf = [0u8; 256];
        // A second ping is suppressed until the first receives a reply.
        let mut total_send_bytes = 0;
        loop {
            match s.poll_output(&mut buf).unwrap() {
                Output::TcpSend { data } => total_send_bytes += data.len(),
                Output::Timeout(_) => break,
                _ => {}
            }
        }
        assert_eq!(total_send_bytes, HEADER_LEN_EXTENDED);
    }

    #[test]
    fn buffer_too_small_does_not_lose_event() {
        let now = Instant::now();
        let mut s = BcSession::default_client(now);
        let body = b"hello world"; // 11 bytes
        let wire = make_wire_message(9042, body);

        s.handle_input(Input::TcpData(now, &wire)).unwrap();

        // First attempt with a buffer that's too small
        let mut tiny_buf = [0u8; 4];
        match s.poll_output(&mut tiny_buf) {
            Err(BcError::BufferTooSmall { needed, available }) => {
                assert_eq!(needed, 11);
                assert_eq!(available, 4);
            }
            other => panic!("expected BufferTooSmall, got {other:?}"),
        }

        // Retry with a sufficiently large buffer -- event should still be there
        let mut buf = [0u8; 256];
        match s.poll_output(&mut buf).unwrap() {
            Output::Event(Event::UnhandledMessage { msg_id, body: b }) => {
                assert_eq!(msg_id, 9042);
                assert_eq!(b, b"hello world");
            }
            other => panic!("expected UnhandledMessage, got {other:?}"),
        }
    }

    fn make_wire_ext(msg_id: u32, body: &[u8], status_class: u32) -> Vec<u8> {
        let has_ext =
            (status_class >> 16) == BC_CLASS_MODERN_EXT as u32 || (status_class >> 16) == 0;
        let header = PacketHeader {
            msg_id,
            body_len: body.len() as u32,
            encryption_offset: body.len() as u32,
            status_class,
            extension: if has_ext { Some(0) } else { None },
        };
        let mut hdr_buf = [0u8; HEADER_LEN_EXTENDED];
        let hdr_len = header.serialize(&mut hdr_buf);
        let mut wire = Vec::new();
        wire.extend_from_slice(&hdr_buf[..hdr_len]);
        wire.extend_from_slice(body);
        wire
    }

    fn test_login_params() -> LoginParams {
        LoginParams {
            username: ArrayString::try_from("admin").unwrap(),
            password: ArrayString::try_from("secret").unwrap(),
            encryption: EncryptionMode::Aes,
        }
    }

    #[test]
    fn login_command_sends_login_upgrade() {
        let now = Instant::now();
        let mut s = BcSession::default_client(now);
        let params = test_login_params();

        s.handle_input(Input::Command(Command::Login(params)))
            .unwrap();
        assert_eq!(s.state(), SessionState::AwaitingNonce);

        let mut buf = [0u8; 256];
        match s.poll_output(&mut buf).unwrap() {
            Output::TcpSend { data } => {
                let (h, _hdr_len) = PacketHeader::parse(data).unwrap();
                assert_eq!(h.msg_id, crate::COMMAND_LOGIN);
                assert_eq!(h.body_len, 0); // header-only LoginUpgrade
                assert!(h.is_binary()); // uses LEGACY class
                // response_code encodes requested encryption mode
                assert_eq!(
                    h.response_code(),
                    EncryptionMode::Aes.to_class_value() as u16
                );
            }
            other => panic!("expected TcpSend, got {other:?}"),
        }
    }

    #[test]
    fn header_stream_id_overrides_ambiguous_channel_codec_cache() {
        let now = Instant::now();
        let mut session = BcSession::default_client(now);
        let main = StreamSubscriptionEntry {
            channel: 0,
            stream_type: crate::stream::StreamType::Main,
            expected_width: 2560,
            expected_height: 1440,
        };
        let sub = StreamSubscriptionEntry {
            channel: 0,
            stream_type: crate::stream::StreamType::Sub,
            expected_width: 640,
            expected_height: 360,
        };
        session.stream_subs_by_id.insert(1, main);
        session.stream_subs_by_id.insert(2, sub);
        session
            .stream_id_by_channel_codec
            .insert((0, crate::media::VideoCodec::H264), 1);

        assert_eq!(
            session.resolve_video_stream_id(2, 0, 0, crate::media::VideoCodec::H264),
            2
        );
    }

    #[test]
    fn login_wrong_role() {
        let now = Instant::now();
        let mut s = BcSession::default_camera(now);
        let params = test_login_params();
        let result = s.handle_input(Input::Command(Command::Login(params)));
        assert!(matches!(result, Err(BcError::WrongRole)));
    }

    #[test]
    fn login_already_in_progress() {
        let now = Instant::now();
        let mut s = BcSession::default_client(now);
        let params = test_login_params();
        s.handle_input(Input::Command(Command::Login(params)))
            .unwrap();
        // Second login while AwaitingNonce should fail
        let result = s.handle_input(Input::Command(Command::Login(params)));
        assert!(result.is_err());
    }

    #[test]
    fn nonce_response_triggers_modern_login() {
        let now = Instant::now();
        let mut s = BcSession::default_client(now);
        let params = test_login_params();
        s.handle_input(Input::Command(Command::Login(params)))
            .unwrap();

        // Drain login upgrade TcpSend
        let mut buf = [0u8; 256];
        s.poll_output(&mut buf).unwrap();

        // Feed nonce response (camera → client), body BCEncrypt'd
        let nonce_xml = br#"<body><Encryption version="2"><type>aes</type><nonce>TESTNONCE</nonce></Encryption></body>"#;
        let mut enc_nonce = nonce_xml.to_vec();
        crate::encryption::bc_xor(&mut enc_nonce, 0);
        let wire = make_wire_ext(crate::COMMAND_LOGIN, &enc_nonce, make_status(0, 0xDD02));
        s.handle_input(Input::TcpData(now, &wire)).unwrap();

        assert_eq!(s.state(), SessionState::AwaitingLoginConfirm);

        // Should produce a TcpSend with modern login XML (BCEncrypt'd)
        let mut buf = [0u8; 2048];
        match s.poll_output(&mut buf).unwrap() {
            Output::TcpSend { data } => {
                let (h, hdr_len) = PacketHeader::parse(data).unwrap();
                assert_eq!(h.msg_id, crate::COMMAND_LOGIN);
                assert!(h.is_modern());
                assert!(h.is_extended());
                // response_code is 0 in the modern login (Step 3)
                assert_eq!(h.response_code(), 0);
                assert_eq!(h.bc_class(), BC_CLASS_MODERN_EXT);
                // Body is BCEncrypt'd — decrypt a copy to verify XML
                let mut body = data[hdr_len..].to_vec();
                crate::encryption::bc_xor(&mut body, 0);
                let xml = core::str::from_utf8(&body).unwrap();
                assert!(xml.contains("<LoginUser"));
                assert!(xml.contains("<LoginNet"));
                // Should NOT contain plaintext credentials
                assert!(!xml.contains("admin"));
                assert!(!xml.contains("secret"));
            }
            other => panic!("expected TcpSend, got {other:?}"),
        }
    }

    #[test]
    fn login_confirmation_produces_logged_in() {
        let now = Instant::now();
        let mut s = BcSession::default_client(now);
        let params = test_login_params();
        s.handle_input(Input::Command(Command::Login(params)))
            .unwrap();

        // Drain login upgrade
        let mut buf = [0u8; 2048];
        s.poll_output(&mut buf).unwrap();

        // Nonce response (BCEncrypt'd)
        let nonce_xml = br#"<body><Encryption version="2"><type>aes</type><nonce>NONCE1</nonce></Encryption></body>"#;
        let mut enc_nonce = nonce_xml.to_vec();
        crate::encryption::bc_xor(&mut enc_nonce, 0);
        let nonce_wire = make_wire_ext(crate::COMMAND_LOGIN, &enc_nonce, make_status(0, 0xDD02));
        s.handle_input(Input::TcpData(now, &nonce_wire)).unwrap();

        // Drain modern login send
        while let Ok(Output::TcpSend { .. }) = s.poll_output(&mut buf) {}

        // Login confirmation (BCEncrypt'd)
        let confirm_xml = br#"<body><LoginUser version="2"><userName>admin</userName><result>ok</result><userId>99</userId></LoginUser><DeviceInfo version="2"><model>RLC-810A</model><serialNumber>SN001</serialNumber><firmVer>v3.0</firmVer><channelNum>2</channelNum></DeviceInfo></body>"#;
        let mut enc_confirm = confirm_xml.to_vec();
        crate::encryption::bc_xor(&mut enc_confirm, 0);
        let confirm_wire = make_wire_ext(crate::COMMAND_LOGIN, &enc_confirm, make_status(0, 0));
        s.handle_input(Input::TcpData(now, &confirm_wire)).unwrap();

        assert_eq!(s.state(), SessionState::Connected);

        match s.poll_output(&mut buf).unwrap() {
            Output::Event(Event::LoggedIn(result)) => {
                assert_eq!(result.user_id, 99);
                assert_eq!(result.camera_identity.model.as_str(), "RLC-810A");
                assert_eq!(result.camera_identity.serial.as_str(), "SN001");
                assert_eq!(result.camera_identity.firmware.as_str(), "v3.0");
                assert_eq!(result.camera_identity.channel_count, 2);
                assert_eq!(result.encryption, EncryptionMode::Aes);
            }
            other => panic!("expected LoggedIn event, got {other:?}"),
        }
    }

    #[test]
    fn login_failure_produces_login_failed() {
        let now = Instant::now();
        let mut s = BcSession::default_client(now);
        let params = test_login_params();
        s.handle_input(Input::Command(Command::Login(params)))
            .unwrap();

        // Drain login upgrade
        let mut buf = [0u8; 2048];
        s.poll_output(&mut buf).unwrap();

        // Nonce response (BCEncrypt'd)
        let nonce_xml = br#"<body><Encryption version="2"><type>aes</type><nonce>N1</nonce></Encryption></body>"#;
        let mut enc_nonce = nonce_xml.to_vec();
        crate::encryption::bc_xor(&mut enc_nonce, 0);
        let nonce_wire = make_wire_ext(crate::COMMAND_LOGIN, &enc_nonce, make_status(0, 0xDD02));
        s.handle_input(Input::TcpData(now, &nonce_wire)).unwrap();

        // Drain modern login send
        while let Ok(Output::TcpSend { .. }) = s.poll_output(&mut buf) {}

        // Camera rejects login: no userId in response (BCEncrypt'd)
        let reject_xml = br#"<body><LoginUser><result>failed</result></LoginUser></body>"#;
        let mut enc_reject = reject_xml.to_vec();
        crate::encryption::bc_xor(&mut enc_reject, 0);
        let reject_wire = make_wire_ext(crate::COMMAND_LOGIN, &enc_reject, make_status(0, 0));
        s.handle_input(Input::TcpData(now, &reject_wire)).unwrap();

        assert_eq!(s.state(), SessionState::Disconnected);

        match s.poll_output(&mut buf).unwrap() {
            Output::Event(Event::LoginFailed(_status)) => {}
            other => panic!("expected LoginFailed, got {other:?}"),
        }
    }

    #[test]
    fn encryption_downgrade_on_nonce() {
        let now = Instant::now();
        let mut s = BcSession::default_client(now);
        let params = LoginParams {
            username: ArrayString::try_from("admin").unwrap(),
            password: ArrayString::try_from("pass").unwrap(),
            encryption: EncryptionMode::FullAes,
        };
        s.handle_input(Input::Command(Command::Login(params)))
            .unwrap();

        let mut buf = [0u8; 2048];
        s.poll_output(&mut buf).unwrap(); // drain login upgrade

        // Camera only supports BcEncrypt (BCEncrypt'd nonce body)
        let nonce_xml = br#"<body><Encryption version="1"><type>bc</type><nonce>ABC</nonce></Encryption></body>"#;
        let mut enc_nonce = nonce_xml.to_vec();
        crate::encryption::bc_xor(&mut enc_nonce, 0);
        let wire = make_wire_ext(crate::COMMAND_LOGIN, &enc_nonce, make_status(0, 0xDD01));
        s.handle_input(Input::TcpData(now, &wire)).unwrap();

        // Modern login should use BcEncrypt (downgraded)
        match s.poll_output(&mut buf).unwrap() {
            Output::TcpSend { data } => {
                let (h, _) = PacketHeader::parse(data).unwrap();
                assert_eq!(h.response_code(), 0);
                assert_eq!(h.bc_class(), BC_CLASS_MODERN_EXT);
            }
            other => panic!("expected TcpSend, got {other:?}"),
        }
    }

    #[test]
    fn logout_sends_message_and_disconnects() {
        let now = Instant::now();
        let mut s = BcSession::default_client(now);
        s.set_state(SessionState::Connected);

        s.handle_input(Input::Command(Command::Logout)).unwrap();
        assert_eq!(s.state(), SessionState::Disconnected);

        let mut buf = [0u8; 256];
        match s.poll_output(&mut buf).unwrap() {
            Output::TcpSend { data } => {
                let (h, _) = PacketHeader::parse(data).unwrap();
                assert_eq!(h.msg_id, crate::COMMAND_LOGOUT);
                assert_eq!(h.body_len, 0);
            }
            other => panic!("expected TcpSend, got {other:?}"),
        }
    }

    #[test]
    fn relogin_fires_after_interval() {
        let now = Instant::now();
        let mut s = BcSession::new(
            BcSessionConfig {
                relogin_interval: Duration::from_secs(10),
                keepalive_interval: Duration::from_secs(999), // disable keepalive
                ..BcSessionConfig::default_client()
            },
            now,
        );
        s.set_state(SessionState::Connected);

        // Simulate login state
        s.login_params = Some(test_login_params());
        s.last_login = Some(now);

        // Not yet time for re-login
        let later = now + Duration::from_secs(5);
        s.handle_input(Input::Timeout(later)).unwrap();
        assert_eq!(s.state(), SessionState::Connected);

        // Now past re-login interval
        let much_later = now + Duration::from_secs(11);
        s.handle_input(Input::Timeout(much_later)).unwrap();
        assert_eq!(s.state(), SessionState::AwaitingNonce);

        // Should have a login upgrade queued
        let mut buf = [0u8; 256];
        match s.poll_output(&mut buf).unwrap() {
            Output::TcpSend { data } => {
                let (h, _) = PacketHeader::parse(data).unwrap();
                assert_eq!(h.msg_id, crate::COMMAND_LOGIN);
                assert_eq!(h.body_len, 0); // header-only LoginUpgrade
            }
            other => panic!("expected TcpSend, got {other:?}"),
        }
    }
}
