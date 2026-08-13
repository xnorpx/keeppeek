//! Runtime-neutral RTSP client data types and protocol components.

pub use self::timeline::Timeline;
use std::num::{NonZeroU16, NonZeroU32};
use url::Url;

mod channel_mapping;
pub mod core;
mod parse;
pub mod rtp;
mod timeline;

/// Policy for interpreting the `rtptime` field in a `PLAY` response.
#[derive(Copy, Clone, Debug, Default, derive_more::Display)]
pub enum InitialTimestampPolicy {
    /// Require timestamps only when multiple streams are configured.
    #[default]
    #[display("default")]
    Default,
    /// Require an RTP timestamp for every configured stream.
    #[display("require")]
    Require,
    /// Start each stream's timeline from its first received RTP packet.
    #[display("ignore")]
    Ignore,
    /// Use RTP timestamps only when every configured stream supplies one.
    #[display("permissive")]
    Permissive,
}

/// Policy for interpreting the `seq` field in a `PLAY` response.
#[derive(Copy, Clone, Debug, Default, derive_more::Display)]
#[non_exhaustive]
pub enum InitialSequenceNumberPolicy {
    /// Ignore suspicious zero and one sequence numbers.
    #[default]
    #[display("default")]
    Default,
    /// Respect a server-provided sequence number.
    #[display("respect")]
    Respect,
    /// Ignore zero and one sequence numbers.
    #[display("ignore-suspicious-values")]
    IgnoreSuspiciousValues,
    /// Derive the sequence number from observed RTP packets.
    #[display("ignore")]
    Ignore,
}

/// Policy for RTCP packets whose SSRC differs from the RTP stream.
#[derive(Copy, Clone, Debug, Default, derive_more::Display)]
#[non_exhaustive]
pub enum UnknownRtcpSsrcPolicy {
    /// Drop RTCP packets from unknown SSRCs.
    #[default]
    #[display("default")]
    Default,
    /// Fail the stream on an unknown RTCP SSRC.
    #[display("abort-session")]
    AbortSession,
    /// Drop RTCP packets from unknown SSRCs.
    #[display("drop-packets")]
    DropPackets,
    /// Process RTCP packets despite an unknown SSRC.
    #[display("process-packets")]
    ProcessPackets,
}

/// Policy for TCP interleaved data on an unassigned channel.
#[derive(Copy, Clone, Debug, Default, derive_more::Display)]
pub enum UnassignedChannelDataPolicy {
    /// Ignore unassigned data unless a known live555 issue is detected.
    #[default]
    #[display("auto")]
    Auto,
    /// Treat unassigned data as a stale live555 session.
    #[display("assume-stale-session")]
    AssumeStaleSession,
    /// Treat unassigned data as a protocol error.
    #[display("error")]
    Error,
    /// Ignore unassigned data.
    #[display("ignore")]
    Ignore,
}

/// Configuration shared by RTP and RTCP validation.
#[derive(Default)]
pub struct SessionOptions {
    pub(crate) unassigned_channel_data: UnassignedChannelDataPolicy,
}

impl SessionOptions {
    /// Sets the policy for TCP interleaved data on unknown channels.
    pub const fn unassigned_channel_data(mut self, policy: UnassignedChannelDataPolicy) -> Self {
        self.unassigned_channel_data = policy;
        self
    }
}

/// Options applied when transitioning configured streams to playback.
#[derive(Default)]
pub struct PlayOptions {
    pub(crate) initial_timestamp: InitialTimestampPolicy,
    pub(crate) initial_seq: InitialSequenceNumberPolicy,
    pub(crate) enforce_timestamps_with_max_jump_secs: Option<NonZeroU32>,
    pub(crate) unknown_rtcp_ssrc: UnknownRtcpSsrcPolicy,
}

impl PlayOptions {
    /// Sets the policy for RTP timestamps in the `PLAY` response.
    pub const fn initial_timestamp(self, initial_timestamp: InitialTimestampPolicy) -> Self {
        Self {
            initial_timestamp,
            ..self
        }
    }

    /// Sets the policy for RTP sequence numbers in the `PLAY` response.
    pub const fn initial_seq(self, initial_seq: InitialSequenceNumberPolicy) -> Self {
        Self {
            initial_seq,
            ..self
        }
    }

    /// Sets the policy for RTCP packets with unknown SSRC values.
    pub const fn unknown_rtcp_ssrc(self, unknown_rtcp_ssrc: UnknownRtcpSsrcPolicy) -> Self {
        Self {
            unknown_rtcp_ssrc,
            ..self
        }
    }

    /// Enforces non-decreasing timestamps with the supplied maximum forward jump.
    pub const fn enforce_timestamps_with_max_jump_secs(self, secs: NonZeroU32) -> Self {
        Self {
            enforce_timestamps_with_max_jump_secs: Some(secs),
            ..self
        }
    }
}

pub(crate) struct Presentation {
    pub streams: Box<[Stream]>,
    pub(crate) base_url: Url,
    pub control: Url,
    pub(crate) tool: Option<Tool>,
}

/// The server identifier declared by the SDP `a=tool` attribute.
#[derive(Eq, PartialEq)]
pub struct Tool(Box<str>);

impl Tool {
    pub fn new(raw: &str) -> Self {
        Self(raw.into())
    }

    /// Returns whether this is a live555 version with the stale TCP descriptor bug.
    pub fn has_live555_tcp_bug(&self) -> bool {
        self.0
            .strip_prefix("LIVE555 Streaming Media v")
            .is_some_and(|version| version > "0000.00.00" && version < "2017.06.04")
    }
}

impl std::fmt::Debug for Tool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&*self.0, formatter)
    }
}

impl std::ops::Deref for Tool {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Information about a single SDP media stream.
pub struct Stream {
    pub(crate) depacketizer: Result<crate::codec::Depacketizer, String>,
    pub(crate) state: StreamState,
    pub(crate) media: Box<str>,
    pub(crate) encoding_name: Box<str>,
    pub(crate) rtp_payload_type: u8,
    pub(crate) clock_rate_hz: u32,
    pub(crate) channels: Option<NonZeroU16>,
    pub(crate) framerate: Option<f32>,
    pub(crate) control: Option<Url>,
}

impl std::fmt::Debug for Stream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Stream")
            .field("media", &self.media)
            .field("control", &self.control.as_ref().map(Url::as_str))
            .field("encoding_name", &self.encoding_name)
            .field("rtp_payload_type", &self.rtp_payload_type)
            .field("clock_rate", &self.clock_rate_hz)
            .field("channels", &self.channels)
            .field("framerate", &self.framerate)
            .field("depacketizer", &self.depacketizer)
            .field("state", &self.state)
            .finish()
    }
}

impl Stream {
    /// Returns the SDP media type.
    pub fn media(&self) -> &str {
        &self.media
    }

    /// Returns the lower-case RTP encoding name.
    pub fn encoding_name(&self) -> &str {
        &self.encoding_name
    }

    /// Returns the RTP payload type.
    pub const fn rtp_payload_type(&self) -> u8 {
        self.rtp_payload_type
    }

    /// Returns the RTP clock rate in hertz.
    pub const fn clock_rate_hz(&self) -> u32 {
        self.clock_rate_hz
    }

    /// Returns the known audio channel count.
    pub const fn channels(&self) -> Option<NonZeroU16> {
        self.channels
    }

    /// Returns the SDP-declared frame rate when present.
    pub const fn framerate(&self) -> Option<f32> {
        self.framerate
    }

    /// Returns the stream-specific RTSP control URL when present.
    pub const fn control(&self) -> Option<&Url> {
        self.control.as_ref()
    }

    /// Returns codec parameters when the SDP or received media supplied them.
    pub fn parameters(&self) -> Option<crate::codec::ParametersRef<'_>> {
        self.depacketizer
            .as_ref()
            .ok()
            .and_then(|depacketizer| depacketizer.parameters())
    }

    /// Returns the configured stream context after a successful `SETUP`.
    pub const fn ctx(&self) -> Option<&crate::StreamContext> {
        match &self.state {
            StreamState::Uninit => None,
            StreamState::Init(init) => Some(&init.ctx),
            StreamState::Playing { ctx, .. } => Some(ctx),
        }
    }
}

#[derive(Debug)]
pub(crate) enum StreamState {
    Uninit,
    Init(StreamStateInit),
    Playing {
        timeline: Timeline,
        rtp_handler: rtp::InorderParser,
        ctx: crate::StreamContext,
    },
}

#[derive(Debug)]
pub(crate) struct StreamStateInit {
    pub(crate) ssrc: Option<u32>,
    pub(crate) initial_seq: Option<u16>,
    pub(crate) initial_rtptime: Option<u32>,
    pub(crate) ctx: crate::StreamContext,
}

/// Username and password credentials for Digest authentication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

/// A validated RTP or RTCP packet emitted by the internal parser.
#[derive(Debug)]
#[non_exhaustive]
pub enum PacketItem {
    Rtp(crate::rtp::ReceivedPacket),
    Rtcp(crate::rtcp::ReceivedCompoundPacket),
}
