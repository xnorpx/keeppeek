//! Runtime-agnostic RTSP control-plane state machine.
//!
//! The caller owns sockets and time. After every [`RtspClient::handle_input`]
//! call, drain [`RtspClient::poll_output`] until it returns [`Output::Timeout`].

use super::channel_mapping::{ChannelMappings, ChannelType};
use crate::rtsp::{
    inputs::{Contiguous, Input as _, Slice as _},
    msg,
    parse::{FeedError, Parser},
};
use bytes::{Buf, Bytes, BytesMut};
use std::{
    collections::VecDeque,
    io::Cursor,
    net::IpAddr,
    time::{Duration, Instant, SystemTime},
};
use url::Url;

const DEFAULT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_USER_AGENT: &str = concat!("retina_", env!("CARGO_PKG_VERSION"));

/// A pair of caller-supplied clocks associated with an I/O event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Time {
    /// A monotonic clock used for deadlines and media ordering.
    pub monotonic: Instant,
    /// A wall clock used exclusively for diagnostics.
    pub wall: SystemTime,
}

/// An opaque identifier assigned to one caller-owned TCP connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TcpConnectionId(u64);

impl TcpConnectionId {
    /// Returns the stable numeric identifier assigned by the client.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A TCP destination requested by the RTSP core.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TcpConnectTarget {
    /// The DNS name or numeric IP address to connect.
    pub host: Box<str>,
    /// The RTSP TCP port.
    pub port: u16,
}

/// Configuration for a runtime-agnostic RTSP client.
#[derive(Clone, Debug)]
pub struct ClientOptions {
    /// Maximum time between an acknowledged control request write and its response.
    pub response_timeout: Duration,
    /// Credentials to use when the RTSP server requests Digest authentication.
    pub credentials: Option<super::Credentials>,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            response_timeout: DEFAULT_RESPONSE_TIMEOUT,
            credentials: None,
        }
    }
}

/// A command issued by the application.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Command {
    /// Open a connection and issue an RTSP `DESCRIBE` request.
    Describe { url: Url },
    /// Set up one described stream using TCP interleaved RTP/RTCP transport.
    Setup { stream: usize },
    /// Set up one described stream using UDP RTP/RTCP transport.
    SetupUdp { stream: usize, client_port: u16 },
    /// Start playback for every configured stream in the presentation.
    Play,
}

/// Identifies the kind of UDP media datagram delivered to the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UdpPacketKind {
    Rtp,
    Rtcp,
}

/// A mutation supplied by the caller.
#[derive(Debug)]
#[non_exhaustive]
pub enum Input<'a> {
    /// Starts a protocol operation at the supplied time.
    Command { time: Time, command: Command },
    /// Reports a successful caller-owned TCP connection.
    TcpConnected {
        time: Time,
        connection: TcpConnectionId,
    },
    /// Reports a failed caller-owned TCP connection attempt.
    TcpConnectFailed {
        time: Time,
        connection: TcpConnectionId,
        error: Box<str>,
    },
    /// Delivers bytes read from a caller-owned TCP connection.
    TcpData {
        time: Time,
        connection: TcpConnectionId,
        data: &'a [u8],
    },
    /// Delivers one RTP or RTCP datagram for a configured UDP stream.
    UdpData {
        time: Time,
        stream: usize,
        kind: UdpPacketKind,
        data: &'a [u8],
    },
    /// Acknowledges that a requested TCP transmission was fully written.
    TcpWriteCompleted {
        time: Time,
        connection: TcpConnectionId,
    },
    /// Reports that the caller-owned TCP connection has closed.
    TcpClosed {
        time: Time,
        connection: TcpConnectionId,
    },
    /// Advances the caller-owned monotonic clock.
    Timeout { time: Time },
}

/// Work or an event emitted by [`RtspClient`].
#[derive(Debug)]
#[non_exhaustive]
pub enum Output {
    /// The caller must open a TCP connection and later report its result.
    OpenTcp {
        connection: TcpConnectionId,
        target: TcpConnectTarget,
    },
    /// The caller must write these bytes and later report completion or closure.
    TcpTransmit {
        connection: TcpConnectionId,
        data: Bytes,
    },
    /// The caller should close this TCP connection.
    CloseTcp { connection: TcpConnectionId },
    /// An observable RTSP lifecycle or protocol event.
    Event(Event),
    /// The next deadline. `None` means no timer is currently required.
    Timeout(Option<Instant>),
}

/// An RTSP protocol event emitted by [`RtspClient`].
#[derive(Debug)]
#[non_exhaustive]
pub enum Event {
    /// A requested TCP connection was established.
    TcpConnected { connection: TcpConnectionId },
    /// A requested TCP connection failed.
    TcpConnectFailed {
        connection: TcpConnectionId,
        error: Box<str>,
    },
    /// The TCP connection closed.
    TcpClosed { connection: TcpConnectionId },
    /// A `DESCRIBE` response was received.
    DescribeResponse {
        connection: TcpConnectionId,
        received_at: Time,
        response: msg::Response,
        body: Bytes,
    },
    /// A `SETUP` response was received.
    SetupResponse {
        connection: TcpConnectionId,
        received_at: Time,
        stream: usize,
        response: msg::Response,
        body: Bytes,
    },
    /// A UDP stream was configured with the server's media endpoint.
    UdpSetup {
        stream: usize,
        source: Option<IpAddr>,
        server_port: u16,
    },
    /// A `PLAY` response was received.
    PlayResponse {
        connection: TcpConnectionId,
        received_at: Time,
        response: msg::Response,
        body: Bytes,
    },
    /// A decoded media frame or RTCP compound packet was received.
    CodecItem(crate::codec::CodecItem),
    /// A non-lifecycle RTSP message was received after the initial response.
    Message {
        connection: TcpConnectionId,
        received_at: Time,
        message: msg::Message,
        body: Bytes,
    },
    /// A request response deadline elapsed.
    RequestTimedOut {
        connection: TcpConnectionId,
        cseq: u32,
    },
}

/// Public state of the initial RTSP control lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientState {
    /// No active RTSP operation exists.
    Idle,
    /// Waiting for a caller-owned TCP connection.
    Connecting,
    /// Waiting for the caller to complete the `DESCRIBE` write.
    AwaitingDescribeWrite,
    /// Waiting for a `DESCRIBE` response.
    AwaitingDescribeResponse,
    /// Waiting for the caller to complete a `SETUP` write.
    AwaitingSetupWrite,
    /// Waiting for a `SETUP` response.
    AwaitingSetupResponse,
    /// Waiting for the caller to complete a `PLAY` write.
    AwaitingPlayWrite,
    /// Waiting for a `PLAY` response.
    AwaitingPlayResponse,
    /// A successful `DESCRIBE` response was received.
    Described,
    /// A successful `PLAY` response was received.
    Playing,
    /// The active TCP connection was closed.
    Closed,
    /// The active operation ended with a terminal failure.
    Failed,
}

/// Runtime-neutral errors from invalid caller/core interaction.
#[derive(Clone, Debug, PartialEq, Eq, derive_more::Display)]
pub enum CoreError {
    /// A command or transport outcome was invalid for the current client state.
    #[display("invalid RTSP client state: {_0}")]
    InvalidState(String),
    /// The requested RTSP URL could not be used by the control client.
    #[display("invalid RTSP URL: {_0}")]
    InvalidUrl(String),
    /// The caller supplied a wall clock outside the supported range.
    #[display("invalid RTSP event time: {_0}")]
    InvalidTime(String),
    /// The RTSP TCP byte stream was malformed.
    #[display("RTSP framing error: {_0}")]
    Framing(String),
    /// The server response did not match the pending request.
    #[display("unexpected RTSP response: {_0}")]
    UnexpectedResponse(String),
    /// Encoding a known-valid RTSP request unexpectedly failed.
    #[display("unable to encode RTSP request: {_0}")]
    Encoding(String),
}

impl std::error::Error for CoreError {}

/// A complete RTSP message extracted from a TCP byte stream.
#[derive(Debug)]
pub struct FramedMessage {
    /// Parsed RTSP message head.
    pub message: msg::Message,
    /// Complete RTSP body bytes.
    pub body: Bytes,
}

/// Incremental, runtime-neutral RTSP TCP framer.
#[derive(Default)]
pub struct RtspFramer {
    parser: Parser,
    buffer: BytesMut,
}

impl RtspFramer {
    /// Appends TCP bytes and returns every newly completed RTSP message.
    pub fn push(&mut self, data: &[u8]) -> Result<Vec<FramedMessage>, CoreError> {
        self.buffer.extend_from_slice(data);
        let mut messages = Vec::new();

        loop {
            let initial_len = self.buffer.len();
            let mut input = Contiguous::new(&self.buffer, true);
            let parsed = self.parser.feed(&mut input);
            let consumed = initial_len - input.len();

            match parsed {
                Ok(Some((message, body))) => {
                    let body = Bytes::copy_from_slice(&body.to_cow());
                    self.buffer.advance(consumed);
                    messages.push(FramedMessage { message, body });
                }
                Ok(None) | Err(FeedError::Incomplete(_)) => {
                    if consumed > 0 {
                        self.buffer.advance(consumed);
                    }
                    break;
                }
                Err(error) => return Err(CoreError::Framing(error.to_string())),
            }
        }

        Ok(messages)
    }
}

enum Phase {
    Idle,
    Connecting {
        connection: TcpConnectionId,
        url: Url,
    },
    AwaitingDescribeWrite {
        connection: TcpConnectionId,
        url: Url,
        cseq: u32,
    },
    AwaitingDescribeResponse {
        connection: TcpConnectionId,
        url: Url,
        cseq: u32,
        deadline: Instant,
    },
    AwaitingSetupWrite {
        connection: TcpConnectionId,
        url: Url,
        stream: usize,
        transport: SetupTransport,
        cseq: u32,
    },
    AwaitingSetupResponse {
        connection: TcpConnectionId,
        url: Url,
        stream: usize,
        transport: SetupTransport,
        cseq: u32,
        deadline: Instant,
    },
    AwaitingPlayWrite {
        connection: TcpConnectionId,
        url: Url,
        cseq: u32,
    },
    AwaitingPlayResponse {
        connection: TcpConnectionId,
        url: Url,
        cseq: u32,
        deadline: Instant,
    },
    Described {
        connection: TcpConnectionId,
        url: Url,
    },
    Playing {
        connection: TcpConnectionId,
        url: Url,
    },
    Closed,
    Failed,
}

#[derive(Clone, Copy)]
enum SetupTransport {
    Tcp { channel_id: u8 },
    Udp { client_port: u16 },
}

struct SetupResponseContext {
    connection: TcpConnectionId,
    cseq: u32,
    stream: usize,
    transport: SetupTransport,
    url: Url,
}

/// A runtime-neutral RTSP control-plane client.
pub struct RtspClient {
    options: ClientOptions,
    phase: Phase,
    next_connection: u64,
    next_cseq: u32,
    framer: RtspFramer,
    presentation: Option<super::Presentation>,
    channels: ChannelMappings,
    session: Option<super::parse::SessionHeader>,
    connection_context: Option<crate::ConnectionContext>,
    requested_auth: Option<http_auth::PasswordClient>,
    outputs: VecDeque<Output>,
}

impl Default for RtspClient {
    fn default() -> Self {
        Self::new(ClientOptions::default())
    }
}

impl RtspClient {
    /// Creates a new client with caller-controlled I/O and time.
    pub fn new(options: ClientOptions) -> Self {
        Self {
            options,
            phase: Phase::Idle,
            next_connection: 1,
            next_cseq: 1,
            framer: RtspFramer::default(),
            presentation: None,
            channels: ChannelMappings::default(),
            session: None,
            connection_context: None,
            requested_auth: None,
            outputs: VecDeque::new(),
        }
    }

    /// Returns the current public lifecycle state.
    pub const fn state(&self) -> ClientState {
        match self.phase {
            Phase::Idle => ClientState::Idle,
            Phase::Connecting { .. } => ClientState::Connecting,
            Phase::AwaitingDescribeWrite { .. } => ClientState::AwaitingDescribeWrite,
            Phase::AwaitingDescribeResponse { .. } => ClientState::AwaitingDescribeResponse,
            Phase::AwaitingSetupWrite { .. } => ClientState::AwaitingSetupWrite,
            Phase::AwaitingSetupResponse { .. } => ClientState::AwaitingSetupResponse,
            Phase::AwaitingPlayWrite { .. } => ClientState::AwaitingPlayWrite,
            Phase::AwaitingPlayResponse { .. } => ClientState::AwaitingPlayResponse,
            Phase::Described { .. } => ClientState::Described,
            Phase::Playing { .. } => ClientState::Playing,
            Phase::Closed => ClientState::Closed,
            Phase::Failed => ClientState::Failed,
        }
    }

    /// Returns the described presentation URL after a successful `DESCRIBE` response.
    pub const fn described_url(&self) -> Option<&Url> {
        match &self.phase {
            Phase::Described { url, .. } | Phase::Playing { url, .. } => Some(url),
            _ => None,
        }
    }

    /// Returns the streams parsed from the successful `DESCRIBE` response.
    pub fn streams(&self) -> Option<&[super::Stream]> {
        self.presentation
            .as_ref()
            .map(|presentation| presentation.streams.as_ref())
    }

    /// Applies one caller-supplied mutation.
    pub fn handle_input(&mut self, input: Input<'_>) -> Result<(), CoreError> {
        match input {
            Input::Command { time, command } => self.handle_command(time, command),
            Input::TcpConnected { time, connection } => self.handle_tcp_connected(time, connection),
            Input::TcpConnectFailed {
                connection, error, ..
            } => self.handle_tcp_connect_failed(connection, error),
            Input::TcpData {
                time,
                connection,
                data,
            } => self.handle_tcp_data(time, connection, data),
            Input::UdpData {
                time,
                stream,
                kind,
                data,
            } => self.handle_udp_data(time, stream, kind, data),
            Input::TcpWriteCompleted { time, connection } => {
                self.handle_tcp_write_completed(time, connection)
            }
            Input::TcpClosed { connection, .. } => self.handle_tcp_closed(connection),
            Input::Timeout { time } => self.handle_timeout(time),
        }
    }

    /// Returns the next work item or the next deadline after all work is drained.
    pub fn poll_output(&mut self) -> Output {
        self.outputs
            .pop_front()
            .unwrap_or(Output::Timeout(self.deadline()))
    }

    fn handle_command(&mut self, _time: Time, command: Command) -> Result<(), CoreError> {
        match command {
            Command::Describe { url } => {
                if !matches!(self.phase, Phase::Idle | Phase::Closed | Phase::Failed) {
                    return Err(CoreError::InvalidState(
                        "a control operation is already active".to_string(),
                    ));
                }
                let target = connect_target(&url)?;
                let connection = TcpConnectionId(self.next_connection);
                self.next_connection += 1;
                self.next_cseq = 1;
                self.framer = RtspFramer::default();
                self.presentation = None;
                self.channels = ChannelMappings::default();
                self.session = None;
                self.connection_context = None;
                self.requested_auth = None;
                self.phase = Phase::Connecting { connection, url };
                self.outputs
                    .push_back(Output::OpenTcp { connection, target });
            }
            Command::Setup { stream } => self.start_tcp_setup(stream)?,
            Command::SetupUdp {
                stream,
                client_port,
            } => self.start_udp_setup(stream, client_port)?,
            Command::Play => self.start_play()?,
        }

        Ok(())
    }

    fn handle_tcp_connected(
        &mut self,
        time: Time,
        connection: TcpConnectionId,
    ) -> Result<(), CoreError> {
        let (expected_connection, url) = match &self.phase {
            Phase::Connecting { connection, url } => (*connection, url.clone()),
            _ => {
                return Err(CoreError::InvalidState(
                    "received TCP connection success without a pending connection".to_string(),
                ));
            }
        };
        if connection != expected_connection {
            return Err(unexpected_connection(expected_connection, connection));
        }

        self.connection_context = Some(
            crate::ConnectionContext::unspecified_at(time.wall)
                .map_err(|error| CoreError::InvalidTime(error.to_string()))?,
        );
        let cseq = self.take_cseq()?;
        let authorization = self.authorization(&url, &msg::Method::DESCRIBE)?;
        let data = describe_request(&url, cseq, authorization)?;
        self.phase = Phase::AwaitingDescribeWrite {
            connection,
            url,
            cseq,
        };
        self.outputs
            .push_back(Output::Event(Event::TcpConnected { connection }));
        self.outputs
            .push_back(Output::TcpTransmit { connection, data });
        Ok(())
    }

    fn handle_tcp_connect_failed(
        &mut self,
        connection: TcpConnectionId,
        error: Box<str>,
    ) -> Result<(), CoreError> {
        let expected_connection = phase_connection(&self.phase).ok_or_else(|| {
            CoreError::InvalidState(
                "received TCP connection failure without an active connection".to_string(),
            )
        })?;
        if connection != expected_connection {
            return Err(unexpected_connection(expected_connection, connection));
        }

        self.phase = Phase::Failed;
        self.outputs
            .push_back(Output::Event(Event::TcpConnectFailed { connection, error }));
        Ok(())
    }

    fn handle_tcp_write_completed(
        &mut self,
        time: Time,
        connection: TcpConnectionId,
    ) -> Result<(), CoreError> {
        let (expected_connection, next_phase) = match &self.phase {
            Phase::AwaitingDescribeWrite {
                connection,
                url,
                cseq,
            } => (
                *connection,
                Phase::AwaitingDescribeResponse {
                    connection: *connection,
                    url: url.clone(),
                    cseq: *cseq,
                    deadline: time.monotonic + self.options.response_timeout,
                },
            ),
            Phase::AwaitingSetupWrite {
                connection,
                url,
                stream,
                transport,
                cseq,
            } => (
                *connection,
                Phase::AwaitingSetupResponse {
                    connection: *connection,
                    url: url.clone(),
                    stream: *stream,
                    transport: *transport,
                    cseq: *cseq,
                    deadline: time.monotonic + self.options.response_timeout,
                },
            ),
            Phase::AwaitingPlayWrite {
                connection,
                url,
                cseq,
            } => (
                *connection,
                Phase::AwaitingPlayResponse {
                    connection: *connection,
                    url: url.clone(),
                    cseq: *cseq,
                    deadline: time.monotonic + self.options.response_timeout,
                },
            ),
            _ => {
                return Err(CoreError::InvalidState(
                    "received TCP write completion without a pending RTSP request write"
                        .to_string(),
                ));
            }
        };
        if connection != expected_connection {
            return Err(unexpected_connection(expected_connection, connection));
        }

        self.phase = next_phase;
        Ok(())
    }

    fn handle_tcp_data(
        &mut self,
        time: Time,
        connection: TcpConnectionId,
        data: &[u8],
    ) -> Result<(), CoreError> {
        let expected_connection = phase_connection(&self.phase).ok_or_else(|| {
            CoreError::InvalidState("received TCP data without an active connection".to_string())
        })?;
        if connection != expected_connection {
            return Err(unexpected_connection(expected_connection, connection));
        }
        if matches!(self.phase, Phase::AwaitingDescribeWrite { .. }) {
            return Err(CoreError::InvalidState(
                "received TCP data before the DESCRIBE write completed".to_string(),
            ));
        }

        for framed in self.framer.push(data)? {
            self.handle_framed_message(time, connection, framed)?;
        }
        Ok(())
    }

    fn handle_framed_message(
        &mut self,
        time: Time,
        connection: TcpConnectionId,
        framed: FramedMessage,
    ) -> Result<(), CoreError> {
        match &self.phase {
            Phase::AwaitingDescribeResponse {
                connection: expected_connection,
                cseq,
                url,
                ..
            } => {
                let expected_connection = *expected_connection;
                let cseq = *cseq;
                let url = url.clone();
                if connection != expected_connection {
                    return Err(unexpected_connection(expected_connection, connection));
                }
                self.handle_describe_response(time, connection, cseq, url, framed)
            }
            Phase::AwaitingSetupResponse {
                connection: expected_connection,
                cseq,
                stream,
                transport,
                url,
                ..
            } => {
                let expected_connection = *expected_connection;
                let cseq = *cseq;
                let stream = *stream;
                let transport = *transport;
                let url = url.clone();
                if connection != expected_connection {
                    return Err(unexpected_connection(expected_connection, connection));
                }
                self.handle_setup_response(
                    time,
                    SetupResponseContext {
                        connection,
                        cseq,
                        stream,
                        transport,
                        url,
                    },
                    framed,
                )
            }
            Phase::AwaitingPlayResponse {
                connection: expected_connection,
                cseq,
                url,
                ..
            } => {
                let expected_connection = *expected_connection;
                let cseq = *cseq;
                let url = url.clone();
                if connection != expected_connection {
                    return Err(unexpected_connection(expected_connection, connection));
                }
                if matches!(framed.message, msg::Message::Data(_)) {
                    return Ok(());
                }
                self.handle_play_response(time, connection, cseq, url, framed)
            }
            Phase::Described {
                connection: expected_connection,
                ..
            } => {
                if connection != *expected_connection {
                    return Err(unexpected_connection(*expected_connection, connection));
                }
                self.outputs.push_back(Output::Event(Event::Message {
                    connection,
                    received_at: time,
                    message: framed.message,
                    body: framed.body,
                }));
                Ok(())
            }
            Phase::Playing {
                connection: expected_connection,
                ..
            } => {
                if connection != *expected_connection {
                    return Err(unexpected_connection(*expected_connection, connection));
                }
                self.handle_playing_message(time, connection, framed)
            }
            _ => Err(CoreError::InvalidState(
                "received a complete RTSP message in the current state".to_string(),
            )),
        }
    }

    fn handle_describe_response(
        &mut self,
        time: Time,
        connection: TcpConnectionId,
        cseq: u32,
        url: Url,
        framed: FramedMessage,
    ) -> Result<(), CoreError> {
        let (response, body) = expected_response(framed, cseq, "DESCRIBE")?;
        if response.status_code == msg::StatusCode::UNAUTHORIZED {
            if let Err(error) = self.accept_auth_challenge(&response) {
                self.phase = Phase::Failed;
                self.outputs.push_back(Output::CloseTcp { connection });
                self.outputs
                    .push_back(Output::Event(Event::DescribeResponse {
                        connection,
                        received_at: time,
                        response,
                        body,
                    }));
                return Err(error);
            }
            let retry_cseq = self.take_cseq()?;
            let authorization = self.authorization(&url, &msg::Method::DESCRIBE)?;
            let data = describe_request(&url, retry_cseq, authorization)?;
            self.phase = Phase::AwaitingDescribeWrite {
                connection,
                url,
                cseq: retry_cseq,
            };
            self.outputs
                .push_back(Output::Event(Event::DescribeResponse {
                    connection,
                    received_at: time,
                    response,
                    body,
                }));
            self.outputs
                .push_back(Output::TcpTransmit { connection, data });
            return Ok(());
        }
        if response.status_code.is_success() {
            match super::parse::parse_describe(url.clone(), &response, &body) {
                Ok(presentation) => {
                    self.presentation = Some(presentation);
                    self.phase = Phase::Described { connection, url };
                }
                Err(description) => {
                    self.phase = Phase::Failed;
                    self.outputs.push_back(Output::CloseTcp { connection });
                    self.outputs
                        .push_back(Output::Event(Event::DescribeResponse {
                            connection,
                            received_at: time,
                            response,
                            body,
                        }));
                    return Err(CoreError::UnexpectedResponse(format!(
                        "unable to parse DESCRIBE response: {description}"
                    )));
                }
            }
        } else {
            self.phase = Phase::Failed;
            self.outputs.push_back(Output::CloseTcp { connection });
        }
        self.outputs
            .push_back(Output::Event(Event::DescribeResponse {
                connection,
                received_at: time,
                response,
                body,
            }));
        Ok(())
    }

    fn handle_playing_message(
        &mut self,
        time: Time,
        connection: TcpConnectionId,
        framed: FramedMessage,
    ) -> Result<(), CoreError> {
        let msg::Message::Data(data) = framed.message else {
            self.outputs.push_back(Output::Event(Event::Message {
                connection,
                received_at: time,
                message: framed.message,
                body: framed.body,
            }));
            return Ok(());
        };
        if self.channels.lookup(data.channel_id).is_none() {
            self.outputs.push_back(Output::Event(Event::Message {
                connection,
                received_at: time,
                message: msg::Message::Data(data),
                body: framed.body,
            }));
            return Ok(());
        }
        let items = self.decode_interleaved_rtp(time, data.channel_id, framed.body)?;
        for item in items {
            self.outputs
                .push_back(Output::Event(Event::CodecItem(item)));
        }
        Ok(())
    }

    fn decode_interleaved_rtp(
        &mut self,
        time: Time,
        channel_id: u8,
        body: Bytes,
    ) -> Result<Vec<crate::codec::CodecItem>, CoreError> {
        let mapping = self.channels.lookup(channel_id).ok_or_else(|| {
            CoreError::InvalidState("interleaved channel mapping disappeared".to_string())
        })?;
        let connection_context = self.connection_context.ok_or_else(|| {
            CoreError::InvalidState("received media without a TCP connection context".to_string())
        })?;
        let message_context = crate::RtspMessageContext::at(0, time.monotonic, time.wall)
            .map_err(|error| CoreError::InvalidTime(error.to_string()))?;
        let packet_context = crate::PacketContext::tcp(message_context);
        let session_options = super::SessionOptions::default();
        let presentation = self.presentation.as_mut().ok_or_else(|| {
            CoreError::InvalidState("received media without a parsed presentation".to_string())
        })?;
        let tool = presentation.tool.as_ref();
        let stream = presentation
            .streams
            .get_mut(mapping.stream_i)
            .ok_or_else(|| {
                CoreError::InvalidState("channel maps to an unknown stream".to_string())
            })?;
        let (timeline, rtp_handler, stream_context) = match &mut stream.state {
            super::StreamState::Playing {
                timeline,
                rtp_handler,
                ctx,
                ..
            } => (timeline, rtp_handler, ctx),
            _ => {
                return Err(CoreError::InvalidState(
                    "received media for a stream that is not playing".to_string(),
                ));
            }
        };
        match mapping.channel_type {
            ChannelType::Rtp => {
                let packet = rtp_handler
                    .rtp(
                        &session_options,
                        stream_context,
                        tool,
                        &connection_context,
                        &packet_context,
                        timeline,
                        mapping.stream_i,
                        body,
                    )
                    .map_err(|error| CoreError::Framing(error.to_string()))?;
                let Some(super::PacketItem::Rtp(packet)) = packet else {
                    return Ok(Vec::new());
                };
                let depacketizer = stream.depacketizer.as_mut().map_err(|description| {
                    CoreError::UnexpectedResponse(format!(
                        "stream {} cannot be depacketized: {description}",
                        mapping.stream_i
                    ))
                })?;
                depacketizer.push(packet).map_err(CoreError::Framing)?;
                let mut items = Vec::new();
                while let Some(item) = depacketizer.pull() {
                    items.push(item.map_err(|error| CoreError::Framing(error.description))?);
                }
                Ok(items)
            }
            ChannelType::Rtcp => {
                let packet = rtp_handler
                    .rtcp(
                        &session_options,
                        stream_context,
                        tool,
                        &connection_context,
                        &packet_context,
                        timeline,
                        mapping.stream_i,
                        body,
                    )
                    .map_err(CoreError::Framing)?;
                Ok(packet
                    .into_iter()
                    .map(|packet| match packet {
                        super::PacketItem::Rtcp(packet) => crate::codec::CodecItem::Rtcp(packet),
                        super::PacketItem::Rtp(_) => unreachable!("RTCP parser returned RTP data"),
                    })
                    .collect())
            }
        }
    }

    fn handle_udp_data(
        &mut self,
        _time: Time,
        stream_id: usize,
        kind: UdpPacketKind,
        data: &[u8],
    ) -> Result<(), CoreError> {
        if !matches!(self.phase, Phase::Playing { .. }) {
            return Ok(());
        }
        let connection_context = self.connection_context.ok_or_else(|| {
            CoreError::InvalidState("received UDP media without a connection context".to_string())
        })?;
        let packet_context = crate::PacketContext::dummy();
        let session_options = super::SessionOptions::default();
        let presentation = self.presentation.as_mut().ok_or_else(|| {
            CoreError::InvalidState("received UDP media without a presentation".to_string())
        })?;
        let tool = presentation.tool.as_ref();
        let stream = presentation.streams.get_mut(stream_id).ok_or_else(|| {
            CoreError::InvalidState(format!("UDP media references unknown stream {stream_id}"))
        })?;
        let (timeline, rtp_handler, stream_context) = match &mut stream.state {
            super::StreamState::Playing {
                timeline,
                rtp_handler,
                ctx,
                ..
            } => (timeline, rtp_handler, ctx),
            _ => {
                return Err(CoreError::InvalidState(format!(
                    "UDP media references inactive stream {stream_id}"
                )));
            }
        };
        let data = Bytes::copy_from_slice(data);
        let items = match kind {
            UdpPacketKind::Rtp => {
                let packet = rtp_handler
                    .rtp(
                        &session_options,
                        stream_context,
                        tool,
                        &connection_context,
                        &packet_context,
                        timeline,
                        stream_id,
                        data,
                    )
                    .map_err(|error| CoreError::Framing(error.to_string()))?;
                let Some(super::PacketItem::Rtp(packet)) = packet else {
                    return Ok(());
                };
                let depacketizer = stream.depacketizer.as_mut().map_err(|description| {
                    CoreError::UnexpectedResponse(format!(
                        "stream {stream_id} cannot be depacketized: {description}"
                    ))
                })?;
                depacketizer.push(packet).map_err(CoreError::Framing)?;
                let mut items = Vec::new();
                while let Some(item) = depacketizer.pull() {
                    items.push(item.map_err(|error| CoreError::Framing(error.description))?);
                }
                items
            }
            UdpPacketKind::Rtcp => rtp_handler
                .rtcp(
                    &session_options,
                    stream_context,
                    tool,
                    &connection_context,
                    &packet_context,
                    timeline,
                    stream_id,
                    data,
                )
                .map_err(CoreError::Framing)?
                .into_iter()
                .map(|packet| match packet {
                    super::PacketItem::Rtcp(packet) => crate::codec::CodecItem::Rtcp(packet),
                    super::PacketItem::Rtp(_) => unreachable!("RTCP parser returned RTP data"),
                })
                .collect(),
        };
        for item in items {
            self.outputs
                .push_back(Output::Event(Event::CodecItem(item)));
        }
        Ok(())
    }

    fn handle_setup_response(
        &mut self,
        time: Time,
        context: SetupResponseContext,
        framed: FramedMessage,
    ) -> Result<(), CoreError> {
        let SetupResponseContext {
            connection,
            cseq,
            stream,
            transport,
            url,
        } = context;
        let (response, body) = expected_response(framed, cseq, "SETUP")?;
        if !response.status_code.is_success() {
            self.phase = Phase::Failed;
            self.outputs.push_back(Output::CloseTcp { connection });
            self.outputs.push_back(Output::Event(Event::SetupResponse {
                connection,
                received_at: time,
                stream,
                response,
                body,
            }));
            return Ok(());
        }

        let setup = match super::parse::parse_setup(&response) {
            Ok(setup) => setup,
            Err(description) => {
                self.phase = Phase::Failed;
                self.outputs.push_back(Output::CloseTcp { connection });
                return Err(CoreError::UnexpectedResponse(format!(
                    "unable to parse SETUP response: {description}"
                )));
            }
        };
        if let Some(existing) = &self.session {
            if existing.id != setup.session.id {
                self.phase = Phase::Failed;
                self.outputs.push_back(Output::CloseTcp { connection });
                return Err(CoreError::UnexpectedResponse(format!(
                    "SETUP changed RTSP session id from {:?} to {:?}",
                    existing.id, setup.session.id
                )));
            }
        } else {
            self.session = Some(setup.session);
        }
        let transport_result = match transport {
            SetupTransport::Tcp { .. } => setup.channel_id.map_or_else(
                || {
                    Err(CoreError::UnexpectedResponse(
                        "SETUP response has no interleaved channel assignment".to_string(),
                    ))
                },
                |channel_id| {
                    self.channels
                        .assign(channel_id, stream)
                        .map_err(|description| {
                            CoreError::UnexpectedResponse(format!(
                                "invalid SETUP interleaved channel assignment: {description}"
                            ))
                        })?;
                    Ok((
                        crate::StreamContext(crate::StreamContextInner::Tcp(
                            crate::TcpStreamContext {
                                rtp_channel_id: channel_id,
                            },
                        )),
                        None,
                    ))
                },
            ),
            SetupTransport::Udp { .. } => setup.server_port.map_or_else(
                || {
                    Err(CoreError::UnexpectedResponse(
                        "UDP SETUP response has no server_port assignment".to_string(),
                    ))
                },
                |server_port| {
                    server_port.checked_add(1).ok_or_else(|| {
                        CoreError::UnexpectedResponse(
                            "UDP SETUP response has invalid server_port assignment".to_string(),
                        )
                    })?;
                    Ok((
                        crate::StreamContext::dummy(),
                        Some((setup.source, server_port)),
                    ))
                },
            ),
        };
        let (stream_context, udp_endpoint) = match transport_result {
            Ok(configured) => configured,
            Err(error) => {
                self.phase = Phase::Failed;
                self.outputs.push_back(Output::CloseTcp { connection });
                return Err(error);
            }
        };
        let presentation = self.presentation.as_mut().ok_or_else(|| {
            CoreError::InvalidState("SETUP completed without a parsed presentation".to_string())
        })?;
        let described_stream = presentation.streams.get_mut(stream).ok_or_else(|| {
            CoreError::InvalidState("SETUP completed for an unknown stream".to_string())
        })?;
        described_stream.state = super::StreamState::Init(super::StreamStateInit {
            ssrc: setup.ssrc,
            initial_seq: None,
            initial_rtptime: None,
            ctx: stream_context,
        });
        self.phase = Phase::Described { connection, url };
        if let Some((source, server_port)) = udp_endpoint {
            self.outputs.push_back(Output::Event(Event::UdpSetup {
                stream,
                source,
                server_port,
            }));
        }
        self.outputs.push_back(Output::Event(Event::SetupResponse {
            connection,
            received_at: time,
            stream,
            response,
            body,
        }));
        Ok(())
    }

    fn start_tcp_setup(&mut self, stream: usize) -> Result<(), CoreError> {
        let channel_id = self.channels.next_unassigned().ok_or_else(|| {
            CoreError::InvalidState("no RTSP interleaved channels remain".to_string())
        })?;
        self.start_setup(stream, SetupTransport::Tcp { channel_id })
    }

    fn start_udp_setup(&mut self, stream: usize, client_port: u16) -> Result<(), CoreError> {
        client_port.checked_add(1).ok_or_else(|| {
            CoreError::InvalidState("UDP client port must allow an adjacent RTCP port".to_string())
        })?;
        self.start_setup(stream, SetupTransport::Udp { client_port })
    }

    fn start_setup(&mut self, stream: usize, transport: SetupTransport) -> Result<(), CoreError> {
        let (connection, url) = match &self.phase {
            Phase::Described { connection, url } => (*connection, url.clone()),
            _ => {
                return Err(CoreError::InvalidState(
                    "SETUP requires a successful DESCRIBE response".to_string(),
                ));
            }
        };
        let presentation = self.presentation.as_ref().ok_or_else(|| {
            CoreError::InvalidState("SETUP requires a parsed presentation".to_string())
        })?;
        let described_stream = presentation.streams.get(stream).ok_or_else(|| {
            CoreError::InvalidState(format!("stream index {stream} is not present in the SDP"))
        })?;
        if !matches!(&described_stream.state, super::StreamState::Uninit) {
            return Err(CoreError::InvalidState(format!(
                "stream index {stream} has already been set up"
            )));
        }
        let stream_url = described_stream
            .control
            .as_ref()
            .unwrap_or(&presentation.control)
            .clone();
        let session_id = self.session.as_ref().map(|session| session.id.to_string());
        let cseq = self.take_cseq()?;
        let authorization = self.authorization(&stream_url, &msg::Method::SETUP)?;
        let data = setup_request(
            &stream_url,
            cseq,
            session_id.as_deref(),
            transport,
            authorization,
        )?;
        self.phase = Phase::AwaitingSetupWrite {
            connection,
            url,
            stream,
            transport,
            cseq,
        };
        self.outputs
            .push_back(Output::TcpTransmit { connection, data });
        Ok(())
    }

    fn start_play(&mut self) -> Result<(), CoreError> {
        let (connection, url) = match &self.phase {
            Phase::Described { connection, url } => (*connection, url.clone()),
            _ => {
                return Err(CoreError::InvalidState(
                    "PLAY requires configured streams after DESCRIBE".to_string(),
                ));
            }
        };
        let presentation = self.presentation.as_ref().ok_or_else(|| {
            CoreError::InvalidState("PLAY requires a parsed presentation".to_string())
        })?;
        if !presentation
            .streams
            .iter()
            .any(|stream| matches!(&stream.state, super::StreamState::Init(_)))
        {
            return Err(CoreError::InvalidState(
                "PLAY requires at least one configured stream".to_string(),
            ));
        }
        let session_id = self
            .session
            .as_ref()
            .ok_or_else(|| {
                CoreError::InvalidState("PLAY requires an RTSP session id from SETUP".to_string())
            })?
            .id
            .to_string();
        let control = presentation.control.clone();
        let cseq = self.take_cseq()?;
        let authorization = self.authorization(&control, &msg::Method::PLAY)?;
        let data = play_request(&control, cseq, &session_id, authorization)?;
        self.phase = Phase::AwaitingPlayWrite {
            connection,
            url,
            cseq,
        };
        self.outputs
            .push_back(Output::TcpTransmit { connection, data });
        Ok(())
    }

    fn handle_play_response(
        &mut self,
        time: Time,
        connection: TcpConnectionId,
        cseq: u32,
        url: Url,
        framed: FramedMessage,
    ) -> Result<(), CoreError> {
        let (response, body) = expected_response(framed, cseq, "PLAY")?;
        if !response.status_code.is_success() {
            self.phase = Phase::Failed;
            self.outputs.push_back(Output::CloseTcp { connection });
            self.outputs.push_back(Output::Event(Event::PlayResponse {
                connection,
                received_at: time,
                response,
                body,
            }));
            return Ok(());
        }

        let result = (|| -> Result<(), String> {
            let presentation = self
                .presentation
                .as_mut()
                .ok_or_else(|| "PLAY completed without a parsed presentation".to_string())?;
            super::parse::parse_play(&response, presentation)?;
            let policy = super::PlayOptions::default();
            let setup_streams = presentation
                .streams
                .iter()
                .filter(|stream| matches!(&stream.state, super::StreamState::Init(_)))
                .count();
            if setup_streams == 0 {
                return Err("PLAY completed without configured streams".to_string());
            }
            let all_have_time = presentation
                .streams
                .iter()
                .all(|stream| match &stream.state {
                    super::StreamState::Init(init) => init.initial_rtptime.is_some(),
                    super::StreamState::Uninit => true,
                    super::StreamState::Playing { .. } => false,
                });

            for (stream_index, stream) in presentation.streams.iter_mut().enumerate() {
                let init = match std::mem::replace(&mut stream.state, super::StreamState::Uninit) {
                    super::StreamState::Init(init) => init,
                    super::StreamState::Uninit => continue,
                    super::StreamState::Playing { .. } => {
                        return Err(format!("stream index {stream_index} was already playing"));
                    }
                };
                let initial_rtptime = match policy.initial_timestamp {
                    super::InitialTimestampPolicy::Require
                    | super::InitialTimestampPolicy::Default
                        if setup_streams > 1 && init.initial_rtptime.is_none() =>
                    {
                        return Err(format!(
                            "PLAY response omitted rtptime for configured stream {stream_index}"
                        ));
                    }
                    super::InitialTimestampPolicy::Require
                    | super::InitialTimestampPolicy::Default
                        if setup_streams > 1 =>
                    {
                        init.initial_rtptime
                    }
                    super::InitialTimestampPolicy::Permissive
                        if setup_streams > 1 && all_have_time =>
                    {
                        init.initial_rtptime
                    }
                    _ => None,
                };
                let initial_seq = match (init.initial_seq, policy.initial_seq) {
                    (Some(_), super::InitialSequenceNumberPolicy::Ignore) => None,
                    (
                        Some(0 | 1),
                        super::InitialSequenceNumberPolicy::Default
                        | super::InitialSequenceNumberPolicy::IgnoreSuspiciousValues,
                    ) => None,
                    (Some(sequence), _) => Some(sequence),
                    (None, _) => None,
                };
                let timeline = super::Timeline::new(
                    initial_rtptime,
                    stream.clock_rate_hz,
                    policy.enforce_timestamps_with_max_jump_secs,
                )?;
                stream.state = super::StreamState::Playing {
                    timeline,
                    rtp_handler: super::rtp::InorderParser::new(
                        init.ssrc,
                        initial_seq,
                        policy.unknown_rtcp_ssrc,
                    ),
                    ctx: init.ctx,
                };
            }
            Ok(())
        })();
        if let Err(description) = result {
            self.phase = Phase::Failed;
            self.outputs.push_back(Output::CloseTcp { connection });
            return Err(CoreError::UnexpectedResponse(format!(
                "unable to initialize PLAY response: {description}"
            )));
        }

        self.phase = Phase::Playing { connection, url };
        self.outputs.push_back(Output::Event(Event::PlayResponse {
            connection,
            received_at: time,
            response,
            body,
        }));
        Ok(())
    }

    fn take_cseq(&mut self) -> Result<u32, CoreError> {
        let cseq = self.next_cseq;
        self.next_cseq = self
            .next_cseq
            .checked_add(1)
            .ok_or_else(|| CoreError::InvalidState("RTSP CSeq counter overflowed".to_string()))?;
        Ok(cseq)
    }

    fn accept_auth_challenge(&mut self, response: &msg::Response) -> Result<(), CoreError> {
        if self.requested_auth.is_some() {
            return Err(CoreError::UnexpectedResponse(
                "received Unauthorized after sending Digest authentication".to_string(),
            ));
        }
        if self.options.credentials.is_none() {
            return Err(CoreError::UnexpectedResponse(
                "RTSP server requested authentication without configured credentials".to_string(),
            ));
        }
        let challenge = response.headers.get("WWW-Authenticate").ok_or_else(|| {
            CoreError::UnexpectedResponse(
                "Unauthorized RTSP response has no WWW-Authenticate header".to_string(),
            )
        })?;
        let challenge: &str = challenge;
        self.requested_auth = Some(http_auth::PasswordClient::try_from(challenge).map_err(
            |error| {
                CoreError::UnexpectedResponse(format!(
                    "unable to parse RTSP authentication challenge: {error}"
                ))
            },
        )?);
        Ok(())
    }

    fn authorization(
        &mut self,
        request_uri: &Url,
        method: &msg::Method,
    ) -> Result<Option<msg::HeaderValue>, CoreError> {
        let Some(auth) = self.requested_auth.as_mut() else {
            return Ok(None);
        };
        let credentials = self.options.credentials.as_ref().ok_or_else(|| {
            CoreError::InvalidState(
                "Digest authentication has no configured credentials".to_string(),
            )
        })?;
        let value = auth
            .respond(&http_auth::PasswordParams {
                username: &credentials.username,
                password: &credentials.password,
                uri: request_uri.as_str(),
                method: method.as_ref(),
                body: Some(&[]),
            })
            .map_err(CoreError::Encoding)?;
        msg::HeaderValue::try_from(value)
            .map(Some)
            .map_err(|error| CoreError::Encoding(error.to_string()))
    }

    fn handle_tcp_closed(&mut self, connection: TcpConnectionId) -> Result<(), CoreError> {
        let expected_connection = phase_connection(&self.phase).ok_or_else(|| {
            CoreError::InvalidState("received TCP close without an active connection".to_string())
        })?;
        if connection != expected_connection {
            return Err(unexpected_connection(expected_connection, connection));
        }

        self.phase = Phase::Closed;
        self.outputs
            .push_back(Output::Event(Event::TcpClosed { connection }));
        Ok(())
    }

    fn handle_timeout(&mut self, time: Time) -> Result<(), CoreError> {
        let (connection, cseq, deadline) = match &self.phase {
            Phase::AwaitingDescribeResponse {
                connection,
                cseq,
                deadline,
                ..
            } => (*connection, *cseq, *deadline),
            Phase::AwaitingSetupResponse {
                connection,
                cseq,
                deadline,
                ..
            } => (*connection, *cseq, *deadline),
            Phase::AwaitingPlayResponse {
                connection,
                cseq,
                deadline,
                ..
            } => (*connection, *cseq, *deadline),
            _ => return Ok(()),
        };
        if time.monotonic < deadline {
            return Ok(());
        }

        self.phase = Phase::Failed;
        self.outputs
            .push_back(Output::Event(Event::RequestTimedOut { connection, cseq }));
        self.outputs.push_back(Output::CloseTcp { connection });
        Ok(())
    }

    const fn deadline(&self) -> Option<Instant> {
        match self.phase {
            Phase::AwaitingDescribeResponse { deadline, .. }
            | Phase::AwaitingSetupResponse { deadline, .. }
            | Phase::AwaitingPlayResponse { deadline, .. } => Some(deadline),
            _ => None,
        }
    }
}

fn connect_target(url: &Url) -> Result<TcpConnectTarget, CoreError> {
    if url.scheme() != "rtsp" {
        return Err(CoreError::InvalidUrl(format!(
            "expected rtsp scheme, got {}",
            url.scheme()
        )));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(CoreError::InvalidUrl(
            "credentials must be supplied outside the URL".to_string(),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| CoreError::InvalidUrl("missing host".to_string()))?;
    Ok(TcpConnectTarget {
        host: host.into(),
        port: url.port().unwrap_or(554),
    })
}

fn describe_request(
    url: &Url,
    cseq: u32,
    authorization: Option<msg::HeaderValue>,
) -> Result<Bytes, CoreError> {
    let mut headers = request_headers(cseq, authorization)?;
    headers.insert(
        msg::HeaderName::ACCEPT,
        msg::HeaderValue::try_from("application/sdp")
            .map_err(|error| CoreError::Encoding(error.to_string()))?,
    );
    let request = msg::OwnedMessage::Request {
        head: msg::Request {
            method: msg::Method::DESCRIBE,
            request_uri: Some(url.clone()),
            headers,
        },
        body: Bytes::new(),
    };
    let mut wire = Cursor::new(Vec::new());
    request
        .write(&mut wire)
        .map_err(|error| CoreError::Encoding(error.to_string()))?;
    Ok(Bytes::from(wire.into_inner()))
}

fn setup_request(
    url: &Url,
    cseq: u32,
    session_id: Option<&str>,
    transport: SetupTransport,
    authorization: Option<msg::HeaderValue>,
) -> Result<Bytes, CoreError> {
    let mut headers = request_headers(cseq, authorization)?;
    let transport = match transport {
        SetupTransport::Tcp { channel_id } => format!(
            "RTP/AVP/TCP;unicast;interleaved={channel_id}-{}",
            channel_id + 1
        ),
        SetupTransport::Udp { client_port } => format!(
            "RTP/AVP/UDP;unicast;client_port={client_port}-{}",
            client_port + 1
        ),
    };
    headers.insert(
        msg::HeaderName::TRANSPORT,
        msg::HeaderValue::try_from(transport)
            .map_err(|error| CoreError::Encoding(error.to_string()))?,
    );
    if let Some(session_id) = session_id {
        headers.insert(
            msg::HeaderName::SESSION,
            msg::HeaderValue::try_from(session_id.to_string())
                .map_err(|error| CoreError::Encoding(error.to_string()))?,
        );
    }
    let request = msg::OwnedMessage::Request {
        head: msg::Request {
            method: msg::Method::SETUP,
            request_uri: Some(url.clone()),
            headers,
        },
        body: Bytes::new(),
    };
    let mut wire = Cursor::new(Vec::new());
    request
        .write(&mut wire)
        .map_err(|error| CoreError::Encoding(error.to_string()))?;
    Ok(Bytes::from(wire.into_inner()))
}

fn play_request(
    url: &Url,
    cseq: u32,
    session_id: &str,
    authorization: Option<msg::HeaderValue>,
) -> Result<Bytes, CoreError> {
    let mut headers = request_headers(cseq, authorization)?;
    headers.insert(
        msg::HeaderName::RANGE,
        msg::HeaderValue::try_from("npt=0.000-")
            .map_err(|error| CoreError::Encoding(error.to_string()))?,
    );
    headers.insert(
        msg::HeaderName::SESSION,
        msg::HeaderValue::try_from(session_id.to_string())
            .map_err(|error| CoreError::Encoding(error.to_string()))?,
    );
    let request = msg::OwnedMessage::Request {
        head: msg::Request {
            method: msg::Method::PLAY,
            request_uri: Some(url.clone()),
            headers,
        },
        body: Bytes::new(),
    };
    let mut wire = Cursor::new(Vec::new());
    request
        .write(&mut wire)
        .map_err(|error| CoreError::Encoding(error.to_string()))?;
    Ok(Bytes::from(wire.into_inner()))
}

fn request_headers(
    cseq: u32,
    authorization: Option<msg::HeaderValue>,
) -> Result<msg::Headers, CoreError> {
    let mut headers = msg::Headers::default();
    headers.insert(
        msg::HeaderName::CSEQ,
        msg::HeaderValue::try_from(cseq.to_string())
            .map_err(|error| CoreError::Encoding(error.to_string()))?,
    );
    headers.insert(
        msg::HeaderName::USER_AGENT,
        msg::HeaderValue::try_from(DEFAULT_USER_AGENT)
            .map_err(|error| CoreError::Encoding(error.to_string()))?,
    );
    if let Some(authorization) = authorization {
        headers.insert(msg::HeaderName::AUTHORIZATION, authorization);
    }
    Ok(headers)
}

const fn phase_connection(phase: &Phase) -> Option<TcpConnectionId> {
    match phase {
        Phase::Connecting { connection, .. }
        | Phase::AwaitingDescribeWrite { connection, .. }
        | Phase::AwaitingDescribeResponse { connection, .. }
        | Phase::AwaitingSetupWrite { connection, .. }
        | Phase::AwaitingSetupResponse { connection, .. }
        | Phase::AwaitingPlayWrite { connection, .. }
        | Phase::AwaitingPlayResponse { connection, .. }
        | Phase::Described { connection, .. }
        | Phase::Playing { connection, .. } => Some(*connection),
        Phase::Idle | Phase::Closed | Phase::Failed => None,
    }
}

fn unexpected_connection(expected: TcpConnectionId, actual: TcpConnectionId) -> CoreError {
    CoreError::InvalidState(format!(
        "received connection {} while awaiting connection {}",
        actual.get(),
        expected.get()
    ))
}

fn response_cseq(response: &msg::Response) -> Option<u32> {
    response
        .headers
        .get("CSeq")
        .and_then(|cseq| u32::from_str_radix(cseq, 10).ok())
}

fn expected_response(
    framed: FramedMessage,
    expected_cseq: u32,
    operation: &str,
) -> Result<(msg::Response, Bytes), CoreError> {
    let msg::Message::Response(response) = framed.message else {
        return Err(CoreError::UnexpectedResponse(format!(
            "expected {operation} response"
        )));
    };
    let cseq = response_cseq(&response).ok_or_else(|| {
        CoreError::UnexpectedResponse(format!("{operation} response has no valid CSeq"))
    })?;
    if cseq != expected_cseq {
        return Err(CoreError::UnexpectedResponse(format!(
            "expected {operation} response CSeq {expected_cseq}, got {cseq}"
        )));
    }
    Ok((response, framed.body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::ScriptedDriver;
    use std::time::UNIX_EPOCH;

    const DESCRIBE_RESPONSE: &[u8] = include_bytes!("testdata/hikvision_describe.txt");
    const SETUP_RESPONSE: &[u8] = include_bytes!("testdata/hikvision_setup.txt");
    const PLAY_RESPONSE: &[u8] = include_bytes!("testdata/hikvision_play.txt");
    const UNAUTHORIZED_RESPONSE: &[u8] = include_bytes!("testdata/longse_unauthorized.txt");

    fn time(base: Instant, offset: Duration) -> Time {
        Time {
            monotonic: base + offset,
            wall: UNIX_EPOCH + offset,
        }
    }

    fn describe_client() -> (RtspClient, TcpConnectionId, Instant) {
        describe_client_with_options(ClientOptions::default())
    }

    fn describe_client_with_options(
        options: ClientOptions,
    ) -> (RtspClient, TcpConnectionId, Instant) {
        let start = Instant::now();
        let now = time(start, Duration::ZERO);
        let mut client = RtspClient::new(options);
        client
            .handle_input(Input::Command {
                time: now,
                command: Command::Describe {
                    url: Url::parse("rtsp://camera.example/live").unwrap(),
                },
            })
            .unwrap();
        let connection = match client.poll_output() {
            Output::OpenTcp { connection, target } => {
                assert_eq!(target.host.as_ref(), "camera.example");
                assert_eq!(target.port, 554);
                connection
            }
            output => panic!("expected TCP open work, got {output:?}"),
        };
        (client, connection, start)
    }

    fn described_client() -> (RtspClient, TcpConnectionId, Instant) {
        let (mut client, connection, start) = describe_client();
        client
            .handle_input(Input::TcpConnected {
                time: time(start, Duration::from_millis(1)),
                connection,
            })
            .unwrap();
        let _ = client.poll_output();
        let _ = client.poll_output();
        client
            .handle_input(Input::TcpWriteCompleted {
                time: time(start, Duration::from_millis(2)),
                connection,
            })
            .unwrap();
        let _ = client.poll_output();
        client
            .handle_input(Input::TcpData {
                time: time(start, Duration::from_millis(3)),
                connection,
                data: DESCRIBE_RESPONSE,
            })
            .unwrap();
        let _ = client.poll_output();
        (client, connection, start)
    }

    fn setup_client() -> (RtspClient, TcpConnectionId, Instant) {
        let (mut client, connection, start) = described_client();
        client
            .handle_input(Input::Command {
                time: time(start, Duration::from_millis(4)),
                command: Command::Setup { stream: 0 },
            })
            .unwrap();
        let _ = client.poll_output();
        client
            .handle_input(Input::TcpWriteCompleted {
                time: time(start, Duration::from_millis(5)),
                connection,
            })
            .unwrap();
        let _ = client.poll_output();
        client
            .handle_input(Input::TcpData {
                time: time(start, Duration::from_millis(6)),
                connection,
                data: SETUP_RESPONSE,
            })
            .unwrap();
        let _ = client.poll_output();
        (client, connection, start)
    }

    fn playing_client() -> (RtspClient, TcpConnectionId, Instant) {
        let (mut client, connection, start) = setup_client();
        client
            .handle_input(Input::Command {
                time: time(start, Duration::from_millis(7)),
                command: Command::Play,
            })
            .unwrap();
        let _ = client.poll_output();
        client
            .handle_input(Input::TcpWriteCompleted {
                time: time(start, Duration::from_millis(8)),
                connection,
            })
            .unwrap();
        let _ = client.poll_output();
        client
            .handle_input(Input::TcpData {
                time: time(start, Duration::from_millis(9)),
                connection,
                data: PLAY_RESPONSE,
            })
            .unwrap();
        let _ = client.poll_output();
        (client, connection, start)
    }

    #[test]
    fn describe_emits_connect_then_describe_request() {
        let (mut client, connection, start) = describe_client();
        client
            .handle_input(Input::TcpConnected {
                time: time(start, Duration::from_millis(1)),
                connection,
            })
            .unwrap();

        assert!(matches!(
            client.poll_output(),
            Output::Event(Event::TcpConnected {
                connection: actual
            }) if actual == connection
        ));
        let wire = match client.poll_output() {
            Output::TcpTransmit {
                connection: actual,
                data,
            } if actual == connection => data,
            output => panic!("expected DESCRIBE transmit, got {output:?}"),
        };
        let wire = std::str::from_utf8(&wire).unwrap();
        assert!(wire.starts_with("DESCRIBE rtsp://camera.example/live RTSP/1.0\r\n"));
        assert!(wire.contains("Accept: application/sdp\r\n"));
        assert!(wire.contains("CSeq: 1\r\n"));

        client
            .handle_input(Input::TcpWriteCompleted {
                time: time(start, Duration::from_millis(2)),
                connection,
            })
            .unwrap();
        assert!(matches!(
            client.poll_output(),
            Output::Timeout(Some(deadline)) if deadline == start + Duration::from_millis(2) + DEFAULT_RESPONSE_TIMEOUT
        ));
    }

    #[test]
    fn describe_response_accepts_every_tcp_chunk_boundary() {
        for split_at in 0..=DESCRIBE_RESPONSE.len() {
            let (mut client, connection, start) = describe_client();
            client
                .handle_input(Input::TcpConnected {
                    time: time(start, Duration::from_millis(1)),
                    connection,
                })
                .unwrap();
            let _ = client.poll_output();
            let _ = client.poll_output();
            client
                .handle_input(Input::TcpWriteCompleted {
                    time: time(start, Duration::from_millis(2)),
                    connection,
                })
                .unwrap();
            let _ = client.poll_output();

            client
                .handle_input(Input::TcpData {
                    time: time(start, Duration::from_millis(3)),
                    connection,
                    data: &DESCRIBE_RESPONSE[..split_at],
                })
                .unwrap();
            client
                .handle_input(Input::TcpData {
                    time: time(start, Duration::from_millis(4)),
                    connection,
                    data: &DESCRIBE_RESPONSE[split_at..],
                })
                .unwrap();

            match client.poll_output() {
                Output::Event(Event::DescribeResponse {
                    connection: actual,
                    response,
                    body,
                    ..
                }) => {
                    assert_eq!(actual, connection);
                    assert_eq!(response.status_code, msg::StatusCode::OK);
                    assert!(body.starts_with(b"v=0\r\n"));
                }
                output => panic!("split {split_at}: expected DESCRIBE response, got {output:?}"),
            }
            assert_eq!(client.state(), ClientState::Described);
            let streams = client.streams().expect("successful DESCRIBE has streams");
            assert_eq!(streams.len(), 2);
            assert_eq!(streams[0].media(), "video");
            assert_eq!(streams[0].encoding_name(), "h264");
        }
    }

    #[test]
    fn rejects_data_before_describe_write_completion() {
        let (mut client, connection, start) = describe_client();
        client
            .handle_input(Input::TcpConnected {
                time: time(start, Duration::from_millis(1)),
                connection,
            })
            .unwrap();
        let _ = client.poll_output();
        let _ = client.poll_output();

        let error = client
            .handle_input(Input::TcpData {
                time: time(start, Duration::from_millis(2)),
                connection,
                data: b"RTSP/1.0 200 OK\r\nCSeq: 1\r\n\r\n",
            })
            .unwrap_err();
        assert!(error.to_string().contains("write completed"));
    }

    #[test]
    fn digest_challenge_retries_describe_with_a_fresh_cseq() {
        let options = ClientOptions {
            credentials: Some(crate::client::Credentials {
                username: "operator".to_string(),
                password: "swordfish".to_string(),
            }),
            ..ClientOptions::default()
        };
        let (mut client, connection, start) = describe_client_with_options(options);
        client
            .handle_input(Input::TcpConnected {
                time: time(start, Duration::from_millis(1)),
                connection,
            })
            .unwrap();
        let _ = client.poll_output();
        let initial_wire = match client.poll_output() {
            Output::TcpTransmit { data, .. } => data,
            output => panic!("expected initial DESCRIBE transmit, got {output:?}"),
        };
        assert!(
            !std::str::from_utf8(&initial_wire)
                .unwrap()
                .contains("Authorization:")
        );
        client
            .handle_input(Input::TcpWriteCompleted {
                time: time(start, Duration::from_millis(2)),
                connection,
            })
            .unwrap();
        let _ = client.poll_output();

        client
            .handle_input(Input::TcpData {
                time: time(start, Duration::from_millis(3)),
                connection,
                data: UNAUTHORIZED_RESPONSE,
            })
            .unwrap();
        assert!(matches!(
            client.poll_output(),
            Output::Event(Event::DescribeResponse { response, .. }) if response.status_code == msg::StatusCode::UNAUTHORIZED
        ));
        let retry_wire = match client.poll_output() {
            Output::TcpTransmit {
                connection: actual,
                data,
            } if actual == connection => data,
            output => panic!("expected authenticated DESCRIBE retry, got {output:?}"),
        };
        let retry_wire = std::str::from_utf8(&retry_wire).unwrap();
        assert!(retry_wire.contains("CSeq: 2\r\n"));
        assert!(retry_wire.contains("Authorization: Digest "));
        assert_eq!(client.state(), ClientState::AwaitingDescribeWrite);
    }

    #[test]
    fn digest_challenge_without_credentials_closes_the_connection() {
        let (mut client, connection, start) = describe_client();
        client
            .handle_input(Input::TcpConnected {
                time: time(start, Duration::from_millis(1)),
                connection,
            })
            .unwrap();
        let _ = client.poll_output();
        let _ = client.poll_output();
        client
            .handle_input(Input::TcpWriteCompleted {
                time: time(start, Duration::from_millis(2)),
                connection,
            })
            .unwrap();
        let _ = client.poll_output();

        let error = client
            .handle_input(Input::TcpData {
                time: time(start, Duration::from_millis(3)),
                connection,
                data: UNAUTHORIZED_RESPONSE,
            })
            .unwrap_err();
        assert!(error.to_string().contains("without configured credentials"));
        assert!(matches!(
            client.poll_output(),
            Output::CloseTcp { connection: actual } if actual == connection
        ));
        assert!(matches!(
            client.poll_output(),
            Output::Event(Event::DescribeResponse { response, .. }) if response.status_code == msg::StatusCode::UNAUTHORIZED
        ));
        assert_eq!(client.state(), ClientState::Failed);
    }

    #[test]
    fn describe_timeout_emits_close_work() {
        let (mut client, connection, start) = describe_client();
        client
            .handle_input(Input::TcpConnected {
                time: time(start, Duration::from_millis(1)),
                connection,
            })
            .unwrap();
        let _ = client.poll_output();
        let _ = client.poll_output();
        client
            .handle_input(Input::TcpWriteCompleted {
                time: time(start, Duration::from_millis(2)),
                connection,
            })
            .unwrap();
        let deadline = match client.poll_output() {
            Output::Timeout(Some(deadline)) => deadline,
            output => panic!("expected deadline, got {output:?}"),
        };
        assert_eq!(
            deadline,
            start + Duration::from_millis(2) + DEFAULT_RESPONSE_TIMEOUT
        );

        client
            .handle_input(Input::Timeout {
                time: Time {
                    monotonic: deadline,
                    wall: UNIX_EPOCH + Duration::from_secs(10),
                },
            })
            .unwrap();
        assert!(matches!(
            client.poll_output(),
            Output::Event(Event::RequestTimedOut {
                connection: actual,
                cseq: 1
            }) if actual == connection
        ));
        assert!(matches!(
            client.poll_output(),
            Output::CloseTcp { connection: actual } if actual == connection
        ));
        assert_eq!(client.state(), ClientState::Failed);
    }

    #[test]
    fn framer_preserves_multiple_messages_from_one_input() {
        let mut framer = RtspFramer::default();
        let messages = framer
            .push(b"RTSP/1.0 200 OK\r\nCSeq: 1\r\n\r\nRTSP/1.0 200 OK\r\nCSeq: 2\r\n\r\n")
            .unwrap();
        assert_eq!(messages.len(), 2);
        for (expected_cseq, message) in [1, 2].into_iter().zip(messages) {
            let msg::Message::Response(response) = message.message else {
                panic!("expected response");
            };
            assert_eq!(response_cseq(&response), Some(expected_cseq));
            assert!(message.body.is_empty());
        }
    }

    #[test]
    fn framer_accepts_interleaved_data_at_every_chunk_boundary() {
        const DATA: &[u8] = b"$\x05\x00\x03abc";

        for split_at in 0..=DATA.len() {
            let mut framer = RtspFramer::default();
            let mut messages = framer.push(&DATA[..split_at]).unwrap();
            messages.extend(framer.push(&DATA[split_at..]).unwrap());
            assert_eq!(messages.len(), 1);
            let msg::Message::Data(header) = messages.into_iter().next().unwrap().message else {
                panic!("split {split_at}: expected interleaved data");
            };
            assert_eq!(header.channel_id, 5);
            assert_eq!(header.body_len, 3);
        }
    }

    #[test]
    fn scripted_driver_drains_work_before_reporting_a_deadline() {
        let mut driver = ScriptedDriver::new(RtspClient::default());
        let command_time = driver.time(Duration::ZERO);
        let outputs = driver
            .handle(Input::Command {
                time: command_time,
                command: Command::Describe {
                    url: Url::parse("rtsp://camera.example/live").unwrap(),
                },
            })
            .unwrap();

        assert!(matches!(outputs.first(), Some(Output::OpenTcp { .. })));
        assert!(matches!(outputs.last(), Some(Output::Timeout(None))));
        assert_eq!(driver.client().state(), ClientState::Connecting);
    }

    #[test]
    fn connection_failure_returns_to_a_retryable_failed_state() {
        let (mut client, connection, start) = describe_client();
        client
            .handle_input(Input::TcpConnectFailed {
                time: time(start, Duration::from_millis(1)),
                connection,
                error: "connection refused".into(),
            })
            .unwrap();
        assert!(matches!(
            client.poll_output(),
            Output::Event(Event::TcpConnectFailed {
                connection: actual,
                error,
            }) if actual == connection && error.as_ref() == "connection refused"
        ));
        assert_eq!(client.state(), ClientState::Failed);

        client
            .handle_input(Input::Command {
                time: time(start, Duration::from_millis(2)),
                command: Command::Describe {
                    url: Url::parse("rtsp://camera.example/retry").unwrap(),
                },
            })
            .unwrap();
        assert!(matches!(
            client.poll_output(),
            Output::OpenTcp {
                connection: retry_connection,
                ..
            } if retry_connection.get() == connection.get() + 1
        ));
    }

    #[test]
    fn non_successful_describe_response_emits_close_work() {
        let (mut client, connection, start) = describe_client();
        client
            .handle_input(Input::TcpConnected {
                time: time(start, Duration::from_millis(1)),
                connection,
            })
            .unwrap();
        let _ = client.poll_output();
        let _ = client.poll_output();
        client
            .handle_input(Input::TcpWriteCompleted {
                time: time(start, Duration::from_millis(2)),
                connection,
            })
            .unwrap();
        let _ = client.poll_output();

        client
            .handle_input(Input::TcpData {
                time: time(start, Duration::from_millis(3)),
                connection,
                data: b"RTSP/1.0 404 Not Found\r\nCSeq: 1\r\n\r\n",
            })
            .unwrap();
        assert!(matches!(
            client.poll_output(),
            Output::CloseTcp { connection: actual } if actual == connection
        ));
        assert!(matches!(
            client.poll_output(),
            Output::Event(Event::DescribeResponse { response, .. }) if response.status_code.as_u16() == 404
        ));
        assert_eq!(client.state(), ClientState::Failed);
    }

    #[test]
    fn rejects_describe_response_with_the_wrong_cseq() {
        let (mut client, connection, start) = describe_client();
        client
            .handle_input(Input::TcpConnected {
                time: time(start, Duration::from_millis(1)),
                connection,
            })
            .unwrap();
        let _ = client.poll_output();
        let _ = client.poll_output();
        client
            .handle_input(Input::TcpWriteCompleted {
                time: time(start, Duration::from_millis(2)),
                connection,
            })
            .unwrap();
        let _ = client.poll_output();

        let error = client
            .handle_input(Input::TcpData {
                time: time(start, Duration::from_millis(3)),
                connection,
                data: b"RTSP/1.0 200 OK\r\nCSeq: 2\r\n\r\n",
            })
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("expected DESCRIBE response CSeq 1, got 2")
        );
    }

    #[test]
    fn described_url_is_retained_after_success() {
        let (mut client, connection, start) = describe_client();
        client
            .handle_input(Input::TcpConnected {
                time: time(start, Duration::from_millis(1)),
                connection,
            })
            .unwrap();
        let _ = client.poll_output();
        let _ = client.poll_output();
        client
            .handle_input(Input::TcpWriteCompleted {
                time: time(start, Duration::from_millis(2)),
                connection,
            })
            .unwrap();
        let _ = client.poll_output();
        client
            .handle_input(Input::TcpData {
                time: time(start, Duration::from_millis(3)),
                connection,
                data: DESCRIBE_RESPONSE,
            })
            .unwrap();
        let _ = client.poll_output();

        assert_eq!(
            client.described_url().map(Url::as_str),
            Some("rtsp://camera.example/live")
        );
    }

    #[test]
    fn rejects_non_rtsp_or_credential_urls() {
        for url in [
            "https://camera.example/live",
            "rtsp://admin:secret@camera.example/live",
            "rtsp:///live",
        ] {
            let mut client = RtspClient::default();
            let error = client
                .handle_input(Input::Command {
                    time: time(Instant::now(), Duration::ZERO),
                    command: Command::Describe {
                        url: Url::parse(url).unwrap(),
                    },
                })
                .unwrap_err();
            assert!(matches!(error, CoreError::InvalidUrl(_)));
        }
    }

    #[test]
    fn timeout_before_deadline_does_not_mutate_the_client() {
        let (mut client, connection, start) = describe_client();
        client
            .handle_input(Input::TcpConnected {
                time: time(start, Duration::from_millis(1)),
                connection,
            })
            .unwrap();
        let _ = client.poll_output();
        let _ = client.poll_output();
        client
            .handle_input(Input::TcpWriteCompleted {
                time: time(start, Duration::from_millis(2)),
                connection,
            })
            .unwrap();
        let deadline = match client.poll_output() {
            Output::Timeout(Some(deadline)) => deadline,
            output => panic!("expected deadline, got {output:?}"),
        };

        client
            .handle_input(Input::Timeout {
                time: Time {
                    monotonic: deadline - Duration::from_nanos(1),
                    wall: UNIX_EPOCH + Duration::from_secs(1),
                },
            })
            .unwrap();
        assert!(
            matches!(client.poll_output(), Output::Timeout(Some(actual)) if actual == deadline)
        );
        assert_eq!(client.state(), ClientState::AwaitingDescribeResponse);
    }

    #[test]
    fn tcp_close_is_evented_and_allows_a_new_describe() {
        let (mut client, connection, start) = describe_client();
        client
            .handle_input(Input::TcpClosed {
                time: time(start, Duration::from_millis(1)),
                connection,
            })
            .unwrap();
        assert!(matches!(
            client.poll_output(),
            Output::Event(Event::TcpClosed { connection: actual }) if actual == connection
        ));
        assert_eq!(client.state(), ClientState::Closed);

        client
            .handle_input(Input::Command {
                time: time(start, Duration::from_millis(2)),
                command: Command::Describe {
                    url: Url::parse("rtsp://camera.example/second").unwrap(),
                },
            })
            .unwrap();
        assert!(matches!(client.poll_output(), Output::OpenTcp { .. }));
    }

    #[test]
    fn rejects_transport_events_for_the_wrong_connection() {
        let (mut client, connection, start) = describe_client();
        let error = client
            .handle_input(Input::TcpConnected {
                time: time(start, Duration::from_millis(1)),
                connection: TcpConnectionId(connection.get() + 1),
            })
            .unwrap_err();
        assert!(error.to_string().contains("awaiting connection"));
        assert_eq!(client.state(), ClientState::Connecting);
    }

    #[test]
    fn forwards_interleaved_messages_after_describe() {
        let (mut client, connection, start) = describe_client();
        client
            .handle_input(Input::TcpConnected {
                time: time(start, Duration::from_millis(1)),
                connection,
            })
            .unwrap();
        let _ = client.poll_output();
        let _ = client.poll_output();
        client
            .handle_input(Input::TcpWriteCompleted {
                time: time(start, Duration::from_millis(2)),
                connection,
            })
            .unwrap();
        let _ = client.poll_output();
        client
            .handle_input(Input::TcpData {
                time: time(start, Duration::from_millis(3)),
                connection,
                data: DESCRIBE_RESPONSE,
            })
            .unwrap();
        let _ = client.poll_output();

        let received_at = time(start, Duration::from_millis(4));
        client
            .handle_input(Input::TcpData {
                time: received_at,
                connection,
                data: b"$\x06\x00\x03xyz",
            })
            .unwrap();
        match client.poll_output() {
            Output::Event(Event::Message {
                connection: actual,
                received_at: actual_time,
                message: msg::Message::Data(header),
                body,
            }) => {
                assert_eq!(actual, connection);
                assert_eq!(actual_time, received_at);
                assert_eq!(header.channel_id, 6);
                assert_eq!(&body[..], b"xyz");
            }
            output => panic!("expected forwarded interleaved message, got {output:?}"),
        }
    }

    #[test]
    fn setup_emits_tcp_request_and_records_the_stream_transport() {
        let (mut client, connection, start) = described_client();
        client
            .handle_input(Input::Command {
                time: time(start, Duration::from_millis(4)),
                command: Command::Setup { stream: 0 },
            })
            .unwrap();
        let wire = match client.poll_output() {
            Output::TcpTransmit {
                connection: actual,
                data,
            } if actual == connection => data,
            output => panic!("expected SETUP transmit, got {output:?}"),
        };
        let wire = std::str::from_utf8(&wire).unwrap();
        assert!(wire.starts_with("SETUP rtsp://192.168.5.106:554/Streaming/Channels/101/trackID=1?transportmode=unicast&profile=Profile_1 RTSP/1.0\r\n"));
        assert!(wire.contains("CSeq: 2\r\n"));
        assert!(wire.contains("Transport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n"));

        client
            .handle_input(Input::TcpWriteCompleted {
                time: time(start, Duration::from_millis(5)),
                connection,
            })
            .unwrap();
        assert!(matches!(
            client.poll_output(),
            Output::Timeout(Some(deadline)) if deadline == start + Duration::from_millis(5) + DEFAULT_RESPONSE_TIMEOUT
        ));
        client
            .handle_input(Input::TcpData {
                time: time(start, Duration::from_millis(6)),
                connection,
                data: SETUP_RESPONSE,
            })
            .unwrap();
        assert!(matches!(
            client.poll_output(),
            Output::Event(Event::SetupResponse {
                connection: actual,
                stream: 0,
                response,
                ..
            }) if actual == connection && response.status_code == msg::StatusCode::OK
        ));
        assert_eq!(client.state(), ClientState::Described);
        assert!(client.streams().unwrap()[0].ctx().is_some());
    }

    #[test]
    fn setup_udp_requests_client_ports_and_reports_server_endpoint() {
        let (mut client, connection, start) = described_client();
        client
            .handle_input(Input::Command {
                time: time(start, Duration::from_millis(4)),
                command: Command::SetupUdp {
                    stream: 0,
                    client_port: 50_000,
                },
            })
            .unwrap();
        let wire = match client.poll_output() {
            Output::TcpTransmit { data, .. } => data,
            output => panic!("expected UDP SETUP transmit, got {output:?}"),
        };
        assert!(
            std::str::from_utf8(&wire)
                .unwrap()
                .contains("Transport: RTP/AVP/UDP;unicast;client_port=50000-50001\r\n")
        );

        client
            .handle_input(Input::TcpWriteCompleted {
                time: time(start, Duration::from_millis(5)),
                connection,
            })
            .unwrap();
        let _ = client.poll_output();
        client
            .handle_input(Input::TcpData {
                time: time(start, Duration::from_millis(6)),
                connection,
                data: b"RTSP/1.0 200 OK\r\nCSeq: 2\r\nSession: 1234;timeout=60\r\nTransport: RTP/AVP/UDP;unicast;source=192.168.5.106;server_port=60000-60001;ssrc=30A98EE7\r\n\r\n",
            })
            .unwrap();
        assert!(matches!(
            client.poll_output(),
            Output::Event(Event::UdpSetup {
                stream: 0,
                source: Some(source),
                server_port: 60_000,
            }) if source == "192.168.5.106".parse::<IpAddr>().unwrap()
        ));
        assert!(matches!(
            client.poll_output(),
            Output::Event(Event::SetupResponse { stream: 0, .. })
        ));
        assert_eq!(client.state(), ClientState::Described);
    }

    #[test]
    fn setup_rejects_unknown_or_already_configured_streams() {
        let (mut client, connection, start) = described_client();
        let error = client
            .handle_input(Input::Command {
                time: time(start, Duration::from_millis(4)),
                command: Command::Setup { stream: 2 },
            })
            .unwrap_err();
        assert!(error.to_string().contains("stream index 2"));

        client
            .handle_input(Input::Command {
                time: time(start, Duration::from_millis(5)),
                command: Command::Setup { stream: 0 },
            })
            .unwrap();
        let _ = client.poll_output();
        client
            .handle_input(Input::TcpWriteCompleted {
                time: time(start, Duration::from_millis(6)),
                connection,
            })
            .unwrap();
        let _ = client.poll_output();
        client
            .handle_input(Input::TcpData {
                time: time(start, Duration::from_millis(7)),
                connection,
                data: SETUP_RESPONSE,
            })
            .unwrap();
        let _ = client.poll_output();

        let error = client
            .handle_input(Input::Command {
                time: time(start, Duration::from_millis(8)),
                command: Command::Setup { stream: 0 },
            })
            .unwrap_err();
        assert!(error.to_string().contains("already been set up"));
    }

    #[test]
    fn setup_timeout_emits_close_work() {
        let (mut client, connection, start) = described_client();
        client
            .handle_input(Input::Command {
                time: time(start, Duration::from_millis(4)),
                command: Command::Setup { stream: 0 },
            })
            .unwrap();
        let _ = client.poll_output();
        client
            .handle_input(Input::TcpWriteCompleted {
                time: time(start, Duration::from_millis(5)),
                connection,
            })
            .unwrap();
        let deadline = match client.poll_output() {
            Output::Timeout(Some(deadline)) => deadline,
            output => panic!("expected SETUP deadline, got {output:?}"),
        };
        client
            .handle_input(Input::Timeout {
                time: time(start, deadline.duration_since(start)),
            })
            .unwrap();
        assert!(matches!(
            client.poll_output(),
            Output::Event(Event::RequestTimedOut {
                connection: actual,
                cseq: 2,
            }) if actual == connection
        ));
        assert!(matches!(
            client.poll_output(),
            Output::CloseTcp { connection: actual } if actual == connection
        ));
        assert_eq!(client.state(), ClientState::Failed);
    }

    #[test]
    fn setup_response_without_interleaved_transport_closes_the_connection() {
        let (mut client, connection, start) = described_client();
        client
            .handle_input(Input::Command {
                time: time(start, Duration::from_millis(4)),
                command: Command::Setup { stream: 0 },
            })
            .unwrap();
        let _ = client.poll_output();
        client
            .handle_input(Input::TcpWriteCompleted {
                time: time(start, Duration::from_millis(5)),
                connection,
            })
            .unwrap();
        let _ = client.poll_output();
        let error = client
            .handle_input(Input::TcpData {
                time: time(start, Duration::from_millis(6)),
                connection,
                data: b"RTSP/1.0 200 OK\r\nCSeq: 2\r\nSession: 1234;timeout=60\r\nTransport: RTP/AVP/TCP;unicast\r\n\r\n",
            })
            .unwrap_err();
        assert!(error.to_string().contains("no interleaved channel"));
        assert!(matches!(
            client.poll_output(),
            Output::CloseTcp { connection: actual } if actual == connection
        ));
        assert_eq!(client.state(), ClientState::Failed);
    }

    #[test]
    fn play_emits_request_and_initializes_configured_streams() {
        let (mut client, connection, start) = setup_client();
        client
            .handle_input(Input::Command {
                time: time(start, Duration::from_millis(7)),
                command: Command::Play,
            })
            .unwrap();
        let wire = match client.poll_output() {
            Output::TcpTransmit {
                connection: actual,
                data,
            } if actual == connection => data,
            output => panic!("expected PLAY transmit, got {output:?}"),
        };
        let wire = std::str::from_utf8(&wire).unwrap();
        assert!(wire.starts_with("PLAY rtsp://192.168.5.106:554/Streaming/Channels/101/?transportmode=unicast&profile=Profile_1 RTSP/1.0\r\n"));
        assert!(wire.contains("CSeq: 3\r\n"));
        assert!(wire.contains("Session: 708345999\r\n"));
        assert!(wire.contains("Range: npt=0.000-\r\n"));

        client
            .handle_input(Input::TcpWriteCompleted {
                time: time(start, Duration::from_millis(8)),
                connection,
            })
            .unwrap();
        assert!(matches!(
            client.poll_output(),
            Output::Timeout(Some(deadline)) if deadline == start + Duration::from_millis(8) + DEFAULT_RESPONSE_TIMEOUT
        ));
        client
            .handle_input(Input::TcpData {
                time: time(start, Duration::from_millis(9)),
                connection,
                data: PLAY_RESPONSE,
            })
            .unwrap();
        assert!(matches!(
            client.poll_output(),
            Output::Event(Event::PlayResponse {
                connection: actual,
                response,
                ..
            }) if actual == connection && response.status_code == msg::StatusCode::OK
        ));
        assert_eq!(client.state(), ClientState::Playing);
        assert!(client.streams().unwrap()[0].ctx().is_some());
    }

    #[test]
    fn play_accepts_interleaved_data_before_response() {
        let (mut client, connection, start) = setup_client();
        client
            .handle_input(Input::Command {
                time: time(start, Duration::from_millis(7)),
                command: Command::Play,
            })
            .unwrap();
        let _ = client.poll_output();
        client
            .handle_input(Input::TcpWriteCompleted {
                time: time(start, Duration::from_millis(8)),
                connection,
            })
            .unwrap();
        let _ = client.poll_output();

        client
            .handle_input(Input::TcpData {
                time: time(start, Duration::from_millis(9)),
                connection,
                data: b"\x24\x01\x00\x04rtcp",
            })
            .unwrap();
        assert_eq!(client.state(), ClientState::AwaitingPlayResponse);

        client
            .handle_input(Input::TcpData {
                time: time(start, Duration::from_millis(10)),
                connection,
                data: PLAY_RESPONSE,
            })
            .unwrap();
        assert!(matches!(
            client.poll_output(),
            Output::Event(Event::PlayResponse {
                connection: actual,
                response,
                ..
            }) if actual == connection && response.status_code == msg::StatusCode::OK
        ));
        assert_eq!(client.state(), ClientState::Playing);
    }

    #[test]
    fn play_rejects_a_presentation_without_configured_streams() {
        let (mut client, _connection, start) = described_client();
        let error = client
            .handle_input(Input::Command {
                time: time(start, Duration::from_millis(4)),
                command: Command::Play,
            })
            .unwrap_err();
        assert!(error.to_string().contains("configured stream"));
        assert_eq!(client.state(), ClientState::Described);
    }

    #[test]
    fn play_timeout_emits_close_work() {
        let (mut client, connection, start) = setup_client();
        client
            .handle_input(Input::Command {
                time: time(start, Duration::from_millis(7)),
                command: Command::Play,
            })
            .unwrap();
        let _ = client.poll_output();
        client
            .handle_input(Input::TcpWriteCompleted {
                time: time(start, Duration::from_millis(8)),
                connection,
            })
            .unwrap();
        let deadline = match client.poll_output() {
            Output::Timeout(Some(deadline)) => deadline,
            output => panic!("expected PLAY deadline, got {output:?}"),
        };
        client
            .handle_input(Input::Timeout {
                time: time(start, deadline.duration_since(start)),
            })
            .unwrap();
        assert!(matches!(
            client.poll_output(),
            Output::Event(Event::RequestTimedOut {
                connection: actual,
                cseq: 3,
            }) if actual == connection
        ));
        assert!(matches!(
            client.poll_output(),
            Output::CloseTcp { connection: actual } if actual == connection
        ));
        assert_eq!(client.state(), ClientState::Failed);
    }

    #[test]
    fn interleaved_h264_rtp_emits_a_video_frame_after_play() {
        let (mut client, connection, start) = playing_client();
        let mut rtp = vec![0x80, 0xe0];
        rtp.extend(24_104_u16.to_be_bytes());
        rtp.extend(1_270_711_678_u32.to_be_bytes());
        rtp.extend(0x4cacc3d1_u32.to_be_bytes());
        rtp.extend([0x65, 0x88, 0x84, 0x21]);
        let mut interleaved = vec![b'$', 0];
        interleaved.extend(u16::try_from(rtp.len()).unwrap().to_be_bytes());
        interleaved.extend(rtp);

        client
            .handle_input(Input::TcpData {
                time: time(start, Duration::from_millis(10)),
                connection,
                data: &interleaved,
            })
            .unwrap();
        match client.poll_output() {
            Output::Event(Event::CodecItem(crate::codec::CodecItem::VideoFrame(frame))) => {
                assert!(frame.is_random_access_point());
                assert!(!frame.data().is_empty());
            }
            output => panic!("expected H.264 video frame, got {output:?}"),
        }
    }

    #[test]
    fn malformed_successful_describe_response_closes_the_connection() {
        let (mut client, connection, start) = describe_client();
        client
            .handle_input(Input::TcpConnected {
                time: time(start, Duration::from_millis(1)),
                connection,
            })
            .unwrap();
        let _ = client.poll_output();
        let _ = client.poll_output();
        client
            .handle_input(Input::TcpWriteCompleted {
                time: time(start, Duration::from_millis(2)),
                connection,
            })
            .unwrap();
        let _ = client.poll_output();

        let error = client
            .handle_input(Input::TcpData {
                time: time(start, Duration::from_millis(3)),
                connection,
                data: b"RTSP/1.0 200 OK\r\nCSeq: 1\r\nContent-Type: application/sdp\r\nContent-Length: 3\r\n\r\nv=0",
            })
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unable to parse DESCRIBE response")
        );
        assert!(matches!(
            client.poll_output(),
            Output::CloseTcp { connection: actual } if actual == connection
        ));
        assert!(matches!(
            client.poll_output(),
            Output::Event(Event::DescribeResponse { connection: actual, .. }) if actual == connection
        ));
        assert_eq!(client.state(), ClientState::Failed);
        assert!(client.streams().is_none());
    }
}
