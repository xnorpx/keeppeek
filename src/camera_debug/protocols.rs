//! Bounded live and fake camera protocol capture and transport comparison.
//!
//! Executes real protocol sessions over RTSP/RTSPS (`retina`), ONVIF SOAP
//! (`onvif`), Hikvision ISAPI (`isapi`), Reolink HTTP JSON, and Reolink Baichuan
//! (`reo-proto`), tapping decrypted wire traffic into `capture.pcapng`, raw
//! bitstreams, `muxed.mp4`, `sdp.txt`, and `protocol_transcript.json`.

use crate::{
    camera_debug::{
        bitstream::{
            AudioElementaryCodec, AudioStreamParams, BitstreamAnomalyReport, BitstreamOutputs,
            BitstreamRecordOptions, BitstreamRecorder, CapturedRtpPacket, VideoCodecKind,
            VideoFraming, VideoStreamParams,
        },
        pcapng::{
            FlowDirection, PcapngWriter, TcpFlowKey, format_packet_comment, unix_timestamp_us,
        },
        repro,
        store::{CaptureMetadata, CaptureStore},
        traces::{TraceSession, TraceSessionOptions},
    },
    cameras::CameraTransport,
    logging::LoggingService,
};
use anyhow::{Context, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use onvif::soap::client::{
    AuthType as OnvifAuthType, ClientBuilder as OnvifClientBuilder,
    Credentials as OnvifCredentials, TlsVerification,
};
use reo_proto::{
    BcError, BcSession, BcSessionConfig, Command as BcCommand, Event as BcEvent, Input as BcInput,
    LoginParams, Output as BcOutput, PacketHeader, Role as BcRole, StreamSubscription, StreamType,
    auth::EncryptionMode,
    media::{AudioCodec as BcAudioCodec, VideoCodec as BcVideoCodec},
};
use retina::{
    client::{
        Credentials as RtspCredentials,
        core::{
            ClientOptions, ClientState, Command as RtspCommand, Event as RtspEvent,
            Input as RtspInput, Output as RtspOutput, RtspClient, TcpConnectionId,
            Time as RtspTime, UdpPacketKind,
        },
    },
    codec::{AudioParameters, CodecItem, ParametersRef, VideoParameters, VideoParametersCodec},
};
use rumqttc::tokio_native_tls::native_tls::{TlsConnector, TlsStream};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fmt,
    fs::File,
    io::{BufWriter, Read, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket},
    num::NonZeroU16,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use ureq::{
    Agent,
    tls::{RootCerts, TlsConfig, TlsProvider},
    unversioned::{
        resolver::DefaultResolver,
        transport::{
            Buffers, ConnectionDetails, Connector, NativeTlsConnector, NextTimeout, TcpConnector,
            Transport,
        },
    },
};
use url::Url;
use zeroize::Zeroize;

/// Schema version written into `protocol_transcript.json`.
pub const TRANSCRIPT_SCHEMA_VERSION: &str = "keeppeek.protocol-transcript.v1";
const DEFAULT_DURATION_MAX_MS: u64 = 5_000;
const HARD_DURATION_MAX_MS: u64 = 60_000;
const DEFAULT_FRAMES_MAX: u64 = 120;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const POLL_SLICE: Duration = Duration::from_millis(50);
const TCP_READ_BUF_BYTES: usize = 256 * 1024;
const UDP_READ_BUF_BYTES: usize = 65_536;
const HTTP_RESPONSE_LIMIT_BYTES: u64 = 4 * 1024 * 1024;
const RTSP_INTERLEAVE_MAGIC: u8 = b'$';

/// Protocol family targeted by a camera debug capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureProtocol {
    Rtsp,
    Onvif,
    Isapi,
    ReolinkHttp,
    ReoProto,
}

impl CaptureProtocol {
    /// Returns the canonical lowercase protocol identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rtsp => "rtsp",
            Self::Onvif => "onvif",
            Self::Isapi => "isapi",
            Self::ReolinkHttp => "reolink_http",
            Self::ReoProto => "reo_proto",
        }
    }
}

/// Artifact flags controlling which files [`run_capture`] writes to disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureArtifacts {
    pub pcapng: bool,
    pub video_bitstream: bool,
    pub audio_bitstream: bool,
    pub muxed_mp4: bool,
    pub protocol_transcript: bool,
    pub traces: bool,
}

impl CaptureArtifacts {
    /// Returns an artifact set with every supported capture artifact enabled.
    #[must_use]
    pub const fn all() -> Self {
        Self {
            pcapng: true,
            video_bitstream: true,
            audio_bitstream: true,
            muxed_mp4: true,
            protocol_transcript: true,
            traces: true,
        }
    }
}

impl Default for CaptureArtifacts {
    fn default() -> Self {
        Self::all()
    }
}

/// Target endpoint and options for [`run_capture`].
#[derive(Clone)]
pub struct CaptureRequest {
    pub capture_id: String,
    pub camera_id: String,
    pub protocol: CaptureProtocol,
    pub target_url: String,
    pub username: String,
    pub password: String,
    pub transport: CameraTransport,
    pub include_raw_auth: bool,
    pub trigger_probe: bool,
    pub write_rtp_dump: bool,
    pub tls_accept_invalid_certs: bool,
    pub duration_max_ms: u64,
    pub frames_max: u64,
    pub artifacts: CaptureArtifacts,
    pub logging: Option<LoggingService>,
    pub trace_filter: Option<String>,
}

impl fmt::Debug for CaptureRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let host = Url::parse(&self.target_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned));
        formatter
            .debug_struct("CaptureRequest")
            .field("capture_id", &self.capture_id)
            .field("camera_id", &self.camera_id)
            .field("protocol", &self.protocol)
            .field("target_host", &host)
            .field(
                "has_credentials",
                &(!self.username.is_empty() || !self.password.is_empty()),
            )
            .field("transport", &self.transport)
            .field("include_raw_auth", &self.include_raw_auth)
            .field("trigger_probe", &self.trigger_probe)
            .field("duration_max_ms", &self.duration_max_ms)
            .field("frames_max", &self.frames_max)
            .finish()
    }
}

impl Drop for CaptureRequest {
    fn drop(&mut self) {
        self.target_url.zeroize();
        self.username.zeroize();
        self.password.zeroize();
    }
}

impl CaptureRequest {
    /// Builds a bounded capture request with safe defaults (`include_raw_auth: true`).
    pub fn new(
        capture_id: impl Into<String>,
        camera_id: impl Into<String>,
        protocol: CaptureProtocol,
        target_url: impl Into<String>,
    ) -> Self {
        Self {
            capture_id: capture_id.into(),
            camera_id: camera_id.into(),
            protocol,
            target_url: target_url.into(),
            username: String::new(),
            password: String::new(),
            transport: CameraTransport::Tcp,
            include_raw_auth: true,
            trigger_probe: true,
            write_rtp_dump: true,
            tls_accept_invalid_certs: false,
            duration_max_ms: DEFAULT_DURATION_MAX_MS,
            frames_max: DEFAULT_FRAMES_MAX,
            artifacts: CaptureArtifacts::default(),
            logging: None,
            trace_filter: None,
        }
    }

    fn secret_values(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.username.is_empty() {
            out.push(self.username.clone());
        }
        if !self.password.is_empty() {
            out.push(self.password.clone());
        }
        out
    }
}

/// Complete output summary returned after finishing a bounded capture.
#[derive(Debug, Clone)]
pub struct CaptureResult {
    pub capture_dir: PathBuf,
    pub metadata: CaptureMetadata,
    pub transcript: ProtocolTranscript,
    pub bitstream: BitstreamOutputs,
}

/// Structured wire transcript persisted as `protocol_transcript.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolTranscript {
    pub schema_version: String,
    pub camera_id: String,
    pub camera_host: String,
    pub protocol: CaptureProtocol,
    pub tls_decrypted: bool,
    pub include_raw_auth: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtsp: Option<RtspTranscript>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub http: Vec<HttpExchangeTranscript>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reo_proto: Vec<ReoFrameTranscript>,
}

/// Recorded RTSP control exchanges, SDP text, and raw RTP packets.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RtspTranscript {
    pub sdp: Option<String>,
    pub setup_order: Vec<String>,
    pub exchanges: Vec<RtspExchangeTranscript>,
    pub rtp_packets: Vec<CapturedRtpPacket>,
}

/// Single RTSP request/response exchange captured on the control channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RtspExchangeTranscript {
    pub method: String,
    pub uri: String,
    pub request_headers: Vec<(String, String)>,
    pub request_body: String,
    pub status: u16,
    pub reason: String,
    pub response_headers: Vec<(String, String)>,
    pub response_body: String,
    pub delay_ms: u64,
}

/// Single HTTP/HTTPS exchange captured across ONVIF, ISAPI, or Reolink HTTP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpExchangeTranscript {
    pub method: String,
    pub path: String,
    pub soap_action: Option<String>,
    pub request_headers: Vec<(String, String)>,
    pub request_body_text: Option<String>,
    pub status: u16,
    pub status_reason: String,
    pub response_headers: Vec<(String, String)>,
    pub response_body_text: Option<String>,
    pub response_body_base64: Option<String>,
    pub delay_ms: u64,
}

/// Single decrypted Reolink Baichuan message frame recorded on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReoFrameTranscript {
    pub direction: String,
    pub msg_id: u32,
    pub channel_id: u8,
    pub stream_type: u8,
    pub payload_xml: Option<String>,
    pub payload_base64: Option<String>,
    pub delay_ms: u64,
}

/// Per-transport measurement produced by [`compare_transports`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransportComparisonEntry {
    pub transport: String,
    pub capture_id: String,
    pub success: bool,
    pub error: Option<String>,
    pub duration_ms: u64,
    pub time_to_first_frame_ms: Option<u64>,
    pub total_bytes: u64,
    pub anomalies: BitstreamAnomalyReport,
}

/// Side-by-side transport comparison report across candidate transports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransportComparisonReport {
    pub camera_id: String,
    pub protocol: CaptureProtocol,
    pub recommended_transport: Option<String>,
    pub entries: Vec<TransportComparisonEntry>,
}

/// Executes one bounded camera protocol capture and saves all artifacts in `store`.
///
/// # Errors
/// Returns an error when request parameters are invalid, another capture is active for the
/// same `camera_id`, the protocol handshake fails, or disk quota checks reject the capture.
pub fn run_capture(store: &CaptureStore, req: &CaptureRequest) -> anyhow::Result<CaptureResult> {
    ensure!(
        req.duration_max_ms > 0 && req.duration_max_ms <= HARD_DURATION_MAX_MS,
        "duration_max_ms must be between 1 and {HARD_DURATION_MAX_MS}"
    );
    ensure!(req.frames_max > 0, "frames_max must be positive");
    let url = Url::parse(&req.target_url).context("invalid capture target_url")?;
    let host = url
        .host_str()
        .context("target_url missing host")?
        .to_owned();
    let (guard, capture_dir) = store.begin_capture(&req.camera_id, &req.capture_id)?;
    let run_res = execute_capture_inside_dir(store, &capture_dir, &host, &url, req);
    drop(guard);
    if run_res.is_err() {
        let _ = store.delete_capture(&req.capture_id);
    }
    run_res
}

fn execute_capture_inside_dir(
    store: &CaptureStore,
    capture_dir: &Path,
    host: &str,
    url: &Url,
    req: &CaptureRequest,
) -> anyhow::Result<CaptureResult> {
    let started = Instant::now();
    let created_at = unix_now_secs();
    let max_bytes = store.limits().max_capture_bytes;
    let reserved = reserved_capture_budget(max_bytes);
    let trace_budget = if req.artifacts.protocol_transcript {
        reserved / 2
    } else {
        reserved.saturating_sub(4_096)
    };
    let mut ctx = open_capture_context(capture_dir, max_bytes, reserved, started, req)?;
    let mut transcript = new_transcript(req, host);
    run_protocol_with_traces(
        &mut ctx,
        capture_dir,
        url,
        req,
        trace_budget,
        &mut transcript,
    )?;
    transcript.tls_decrypted = ctx.tls_decrypted;
    let packet_count = ctx.pcap.as_ref().map_or(0, PcapngWriter::packet_count);
    if let Some(pcap) = ctx.pcap.take() {
        pcap.finish()?;
    }
    let warnings = std::mem::take(&mut ctx.warnings);
    let tls_decrypted = ctx.tls_decrypted;
    let bitstream_out = ctx.bitstream.finish()?;
    if req.artifacts.protocol_transcript {
        let transcript_budget = remaining_transcript_budget(capture_dir, max_bytes);
        save_transcript_json_bounded(capture_dir, &mut transcript, transcript_budget)?;
    }
    let mut metadata = CaptureMetadata {
        capture_id: req.capture_id.clone(),
        camera_id: req.camera_id.clone(),
        protocol: req.protocol.as_str().to_owned(),
        created_at_unix_secs: created_at,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        total_bytes: 0,
        packet_count,
        include_raw_auth: req.include_raw_auth,
        trigger_probe: req.trigger_probe,
        tls_decrypted,
        transport: transport_label(req.transport).to_owned(),
        files: collect_dir_filenames(capture_dir)?,
        anomalies: bitstream_out.anomalies.clone(),
        warnings,
    };
    store.finalize_capture(&req.capture_id, &mut metadata)?;
    Ok(CaptureResult {
        capture_dir: capture_dir.to_path_buf(),
        metadata,
        transcript,
        bitstream: bitstream_out,
    })
}

fn run_protocol_with_traces(
    ctx: &mut CaptureContext<'_>,
    capture_dir: &Path,
    url: &Url,
    req: &CaptureRequest,
    trace_budget: u64,
    transcript: &mut ProtocolTranscript,
) -> anyhow::Result<()> {
    let secrets = req.secret_values();
    let trace_session = req.artifacts.traces.then(|| {
        TraceSession::start(TraceSessionOptions {
            logging: req.logging.as_ref(),
            capture_dir,
            camera_id: &req.camera_id,
            capture_id: &req.capture_id,
            trace_filter: req.trace_filter.as_deref(),
            budget_bytes: trace_budget,
            include_raw_auth: req.include_raw_auth,
            secrets: &secrets,
        })
    });
    let span = tracing::info_span!(
        "camera_debug_capture",
        camera_id = %req.camera_id,
        capture_id = %req.capture_id,
        protocol = %req.protocol.as_str()
    );
    let dispatch_res = span.in_scope(|| dispatch_protocol_capture(ctx, url, transcript));
    if let Some(session) = trace_session {
        ctx.warnings.extend(session.finish());
    }
    dispatch_res
}

fn reserved_capture_budget(max_bytes: u64) -> u64 {
    (max_bytes / 4)
        .clamp(4_096, 2 * 1024 * 1024)
        .min(max_bytes / 2)
}

fn open_capture_context<'a>(
    capture_dir: &Path,
    max_bytes: u64,
    reserved: u64,
    started: Instant,
    req: &'a CaptureRequest,
) -> anyhow::Result<CaptureContext<'a>> {
    let media_budget = max_bytes.saturating_sub(reserved).max(1_024);
    let pcap_budget = media_budget / 2;
    let bitstream_budget = media_budget.saturating_sub(pcap_budget).max(512);
    let pcap = req
        .artifacts
        .pcapng
        .then(|| {
            PcapngWriter::new(
                BufWriter::new(File::create(capture_dir.join("capture.pcapng"))?),
                pcap_budget,
            )
        })
        .transpose()?;
    let bitstream = BitstreamRecorder::with_options(
        capture_dir,
        bitstream_budget,
        BitstreamRecordOptions {
            write_video: req.artifacts.video_bitstream,
            write_audio: req.artifacts.audio_bitstream,
            write_muxed_mp4: req.artifacts.muxed_mp4,
            write_rtp_dump: req.write_rtp_dump && req.artifacts.protocol_transcript,
        },
    )?;
    Ok(CaptureContext {
        req,
        capture_dir: capture_dir.to_path_buf(),
        started,
        pcap,
        bitstream,
        tls_decrypted: false,
        warnings: Vec::new(),
    })
}

fn new_transcript(req: &CaptureRequest, host: &str) -> ProtocolTranscript {
    ProtocolTranscript {
        schema_version: TRANSCRIPT_SCHEMA_VERSION.to_owned(),
        camera_id: req.camera_id.clone(),
        camera_host: host.to_owned(),
        protocol: req.protocol,
        tls_decrypted: false,
        include_raw_auth: req.include_raw_auth,
        rtsp: None,
        http: Vec::new(),
        reo_proto: Vec::new(),
    }
}

fn dispatch_protocol_capture(
    ctx: &mut CaptureContext<'_>,
    url: &Url,
    transcript: &mut ProtocolTranscript,
) -> anyhow::Result<()> {
    match ctx.req.protocol {
        CaptureProtocol::Rtsp => transcript.rtsp = Some(capture_rtsp(ctx, url)?),
        CaptureProtocol::Onvif => transcript.http = capture_onvif(ctx, url)?,
        CaptureProtocol::Isapi => transcript.http = capture_isapi(ctx, url)?,
        CaptureProtocol::ReolinkHttp => transcript.http = capture_reolink_http(ctx, url)?,
        CaptureProtocol::ReoProto => transcript.reo_proto = capture_reo_proto(ctx, url)?,
    }
    Ok(())
}

/// Runs bounded captures across candidate transports and recommends the cleanest transport.
///
/// # Errors
/// Returns an error if `transports` is empty or exceeds 4 entries.
pub fn compare_transports(
    store: &CaptureStore,
    base_req: &CaptureRequest,
    transports: &[CameraTransport],
) -> anyhow::Result<TransportComparisonReport> {
    ensure!(
        !transports.is_empty() && transports.len() <= 4,
        "transports must contain between 1 and 4 candidates"
    );
    let mut entries = Vec::with_capacity(transports.len());
    for (idx, &transport) in transports.iter().enumerate() {
        let mut req = base_req.clone();
        req.transport = transport;
        req.capture_id = format!(
            "{}-{}-{}",
            base_req.capture_id,
            transport_label(transport),
            idx
        );
        let started = Instant::now();
        match run_capture(store, &req) {
            Ok(result) => {
                let first_ms = result
                    .transcript
                    .rtsp
                    .as_ref()
                    .and_then(|rtsp| rtsp.rtp_packets.first())
                    .map(|pkt| pkt.delay_ms);
                entries.push(TransportComparisonEntry {
                    transport: transport_label(transport).to_owned(),
                    capture_id: req.capture_id.clone(),
                    success: true,
                    error: None,
                    duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    time_to_first_frame_ms: first_ms,
                    total_bytes: result.metadata.total_bytes,
                    anomalies: result.bitstream.anomalies,
                });
            }
            Err(error) => {
                entries.push(TransportComparisonEntry {
                    transport: transport_label(transport).to_owned(),
                    capture_id: req.capture_id.clone(),
                    success: false,
                    error: Some(error.to_string()),
                    duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    time_to_first_frame_ms: None,
                    total_bytes: 0,
                    anomalies: BitstreamAnomalyReport::default(),
                });
            }
        }
    }
    let recommended = pick_recommended_transport(&entries);
    Ok(TransportComparisonReport {
        camera_id: base_req.camera_id.clone(),
        protocol: base_req.protocol,
        recommended_transport: recommended,
        entries,
    })
}

fn pick_recommended_transport(entries: &[TransportComparisonEntry]) -> Option<String> {
    entries
        .iter()
        .filter(|entry| entry.success)
        .min_by_key(|entry| {
            let a = &entry.anomalies;
            let penalty = a.rtp_seq_gaps
                + a.fua_start_errors
                + a.fua_end_errors
                + a.dts_pts_regressions
                + u64::from(a.missing_idr) * 100
                + a.malformed_rtp_packets;
            let no_frames = u64::from(a.video_frames == 0 && a.audio_frames == 0) * 1_000;
            (
                penalty + no_frames,
                entry.transport != "tcp",
                entry.time_to_first_frame_ms.unwrap_or(u64::MAX),
            )
        })
        .map(|entry| entry.transport.clone())
}

struct CaptureContext<'a> {
    req: &'a CaptureRequest,
    capture_dir: PathBuf,
    started: Instant,
    pcap: Option<PcapngWriter<BufWriter<File>>>,
    bitstream: BitstreamRecorder,
    tls_decrypted: bool,
    warnings: Vec<String>,
}

impl CaptureContext<'_> {
    fn write_tcp_pcap(
        &mut self,
        flow: TcpFlowKey,
        dir: FlowDirection,
        payload: &[u8],
        comment: &str,
    ) -> anyhow::Result<()> {
        if let Some(pcap) = self.pcap.as_mut() {
            pcap.write_tcp_payload(flow, dir, unix_timestamp_us(), payload, Some(comment))?;
        }
        Ok(())
    }

    fn close_tcp_pcap(&mut self, flow: TcpFlowKey) {
        if let Some(pcap) = self.pcap.as_mut() {
            let _ = pcap.close_tcp_flow(
                flow,
                FlowDirection::ClientToServer,
                unix_timestamp_us(),
                None,
            );
        }
    }

    fn write_udp_pcap(
        &mut self,
        src: SocketAddr,
        dst: SocketAddr,
        ts: u64,
        data: &[u8],
        comment: &str,
    ) -> anyhow::Result<()> {
        if let Some(pcap) = self.pcap.as_mut() {
            pcap.write_udp_packet(src, dst, ts, data, Some(comment))?;
        }
        Ok(())
    }
}

enum RtspSocket {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl RtspSocket {
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> std::io::Result<()> {
        match self {
            Self::Plain(stream) => stream.set_read_timeout(timeout),
            Self::Tls(stream) => stream.get_mut().set_read_timeout(timeout),
        }
    }

    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        match self {
            Self::Plain(stream) => stream.local_addr(),
            Self::Tls(stream) => stream.get_ref().local_addr(),
        }
    }

    fn peer_addr(&self) -> std::io::Result<SocketAddr> {
        match self {
            Self::Plain(stream) => stream.peer_addr(),
            Self::Tls(stream) => stream.get_ref().peer_addr(),
        }
    }
}

impl Read for RtspSocket {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.read(buf),
            Self::Tls(stream) => stream.read(buf),
        }
    }
}

impl Write for RtspSocket {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(stream) => stream.write(buf),
            Self::Tls(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Plain(stream) => stream.flush(),
            Self::Tls(stream) => stream.flush(),
        }
    }
}

struct RtspUdpPair {
    rtp: UdpSocket,
    rtcp: UdpSocket,
    track: String,
    video_codec: Option<VideoCodecKind>,
}

struct RtspDriver<'a, 'b> {
    ctx: &'a mut CaptureContext<'b>,
    client: RtspClient,
    use_tls: bool,
    socket: Option<(TcpConnectionId, RtspSocket, TcpFlowKey)>,
    udp_streams: HashMap<usize, RtspUdpPair>,
    channel_tracks: HashMap<u8, (String, Option<VideoCodecKind>)>,
    stream_codecs: HashMap<usize, TrackCodecInfo>,
    interleave_buf: Vec<u8>,
    tx_wire_buf: Vec<u8>,
    setup_order: Vec<String>,
    pending_requests: Vec<RawHttpMessage>,
    exchanges: Vec<RtspExchangeTranscript>,
    sdp: Option<String>,
    deadline: Instant,
    retina_timeout: Option<Instant>,
    frames_captured: u64,
}

#[derive(Clone, Copy)]
enum TrackCodecInfo {
    Video(VideoCodecKind),
    Audio(AudioStreamParams),
    Other,
}

fn capture_rtsp(ctx: &mut CaptureContext<'_>, raw_url: &Url) -> anyhow::Result<RtspTranscript> {
    let use_tls = raw_url.scheme().eq_ignore_ascii_case("rtsps");
    ensure!(
        use_tls || raw_url.scheme().eq_ignore_ascii_case("rtsp"),
        "RTSP capture requires rtsp:// or rtsps:// URL"
    );
    ctx.tls_decrypted = use_tls;
    let mut rtsp_url = raw_url.clone();
    if use_tls {
        if raw_url.port().is_none() {
            let _ = rtsp_url.set_port(Some(322));
        }
        rtsp_url
            .set_scheme("rtsp")
            .map_err(|()| anyhow::anyhow!("unable to normalize rtsps scheme"))?;
    }
    let username = if ctx.req.username.is_empty() && !rtsp_url.username().is_empty() {
        rtsp_url.username().to_owned()
    } else {
        ctx.req.username.clone()
    };
    let password = if ctx.req.password.is_empty() {
        rtsp_url.password().unwrap_or("").to_owned()
    } else {
        ctx.req.password.clone()
    };
    rtsp_url.set_username("").ok();
    rtsp_url.set_password(None).ok();

    let client = RtspClient::new(ClientOptions {
        credentials: (!username.is_empty() || !password.is_empty())
            .then_some(RtspCredentials { username, password }),
        ..ClientOptions::default()
    });
    let deadline = Instant::now() + Duration::from_millis(ctx.req.duration_max_ms);
    let mut driver = RtspDriver {
        ctx,
        client,
        use_tls,
        socket: None,
        udp_streams: HashMap::new(),
        channel_tracks: HashMap::new(),
        stream_codecs: HashMap::new(),
        interleave_buf: Vec::new(),
        tx_wire_buf: Vec::new(),
        setup_order: Vec::new(),
        pending_requests: Vec::new(),
        exchanges: Vec::new(),
        sdp: None,
        deadline,
        retina_timeout: None,
        frames_captured: 0,
    };
    driver.run(rtsp_url)
}

impl RtspDriver<'_, '_> {
    fn run(&mut self, url: Url) -> anyhow::Result<RtspTranscript> {
        self.apply(RtspInput::Command {
            time: rtsp_now(),
            command: RtspCommand::Describe { url },
        })?;
        self.wait_state(ClientState::Described)?;
        self.configure_described_streams()?;
        if !self.stream_codecs.is_empty() {
            self.apply(RtspInput::Command {
                time: rtsp_now(),
                command: RtspCommand::Play,
            })?;
            self.wait_state(ClientState::Playing)?;
            while Instant::now() < self.deadline && self.frames_captured < self.ctx.req.frames_max {
                if self.pump_io().is_err() {
                    break;
                }
            }
        }
        if let Some((_, _, flow)) = self.socket.take() {
            if !self.ctx.req.include_raw_auth && !self.tx_wire_buf.is_empty() {
                let trailing = std::mem::take(&mut self.tx_wire_buf);
                let scrubbed =
                    redact_wire_bytes_if_needed(&trailing, false, &self.ctx.req.secret_values());
                let comment = format_packet_comment(
                    self.use_tls,
                    &self.ctx.req.camera_id,
                    if self.use_tls { "rtsps" } else { "rtsp" },
                );
                let _ = self.ctx.write_tcp_pcap(
                    flow,
                    FlowDirection::ClientToServer,
                    &scrubbed,
                    &comment,
                );
            }
            if !self.ctx.req.include_raw_auth
                && !self.interleave_buf.is_empty()
                && !self.interleave_buf.starts_with(&[RTSP_INTERLEAVE_MAGIC])
            {
                let trailing = std::mem::take(&mut self.interleave_buf);
                let scrubbed =
                    redact_wire_bytes_if_needed(&trailing, false, &self.ctx.req.secret_values());
                let comment = format_packet_comment(
                    self.use_tls,
                    &self.ctx.req.camera_id,
                    if self.use_tls { "rtsps" } else { "rtsp" },
                );
                let _ = self.ctx.write_tcp_pcap(
                    flow,
                    FlowDirection::ServerToClient,
                    &scrubbed,
                    &comment,
                );
            }
            self.ctx.close_tcp_pcap(flow);
        }
        Ok(RtspTranscript {
            sdp: self.sdp.clone(),
            setup_order: self.setup_order.clone(),
            exchanges: self.exchanges.clone(),
            rtp_packets: self.ctx.bitstream.captured_rtp_packets().to_vec(),
        })
    }

    fn configure_described_streams(&mut self) -> anyhow::Result<()> {
        let streams_len = self
            .client
            .streams()
            .map_or(0, <[retina::client::Stream]>::len);
        let mut next_channel = 0_u8;
        for idx in 0..streams_len {
            let (media, encoding, rate, channels, params) = {
                let stream = &self.client.streams().expect("described")[idx];
                (
                    stream.media().to_owned(),
                    stream.encoding_name().to_owned(),
                    stream.clock_rate_hz(),
                    stream.channels(),
                    classify_stream_params(stream),
                )
            };
            if matches!(params, TrackCodecInfo::Other)
                && !matches!(media.as_str(), "video" | "audio")
            {
                continue;
            }
            let info = resolve_stream_info(
                &media,
                &encoding,
                rate,
                channels,
                params,
                &mut self.ctx.bitstream,
            );
            self.stream_codecs.insert(idx, info);
            let track_name = if media.is_empty() {
                format!("stream{idx}")
            } else {
                media
            };
            self.setup_order.push(track_name.clone());
            let video_hint = match info {
                TrackCodecInfo::Video(kind) => Some(kind),
                _ => None,
            };
            let cmd = match self.ctx.req.transport {
                CameraTransport::Tcp => {
                    self.channel_tracks
                        .insert(next_channel, (track_name, video_hint));
                    next_channel = next_channel.saturating_add(2);
                    RtspCommand::Setup { stream: idx }
                }
                CameraTransport::Udp => {
                    let pair = self.bind_udp_pair(track_name, video_hint)?;
                    let client_port = pair.rtp.local_addr()?.port();
                    self.udp_streams.insert(idx, pair);
                    RtspCommand::SetupUdp {
                        stream: idx,
                        client_port,
                    }
                }
            };
            self.apply(RtspInput::Command {
                time: rtsp_now(),
                command: cmd,
            })?;
            self.wait_state(ClientState::Described)?;
        }
        Ok(())
    }

    fn bind_udp_pair(
        &self,
        track: String,
        video_codec: Option<VideoCodecKind>,
    ) -> anyhow::Result<RtspUdpPair> {
        let local_ip = self
            .socket
            .as_ref()
            .context("RTSP TCP connection missing")?
            .1
            .local_addr()?
            .ip();
        for _ in 0..64 {
            let rtp = UdpSocket::bind(SocketAddr::new(local_ip, 0))?;
            let Some(rtcp_port) = rtp.local_addr()?.port().checked_add(1) else {
                continue;
            };
            if let Ok(rtcp) = UdpSocket::bind(SocketAddr::new(local_ip, rtcp_port)) {
                rtp.set_nonblocking(true)?;
                rtcp.set_nonblocking(true)?;
                return Ok(RtspUdpPair {
                    rtp,
                    rtcp,
                    track,
                    video_codec,
                });
            }
        }
        bail!("unable to bind adjacent UDP RTP/RTCP ports")
    }

    fn apply(&mut self, input: RtspInput<'_>) -> anyhow::Result<()> {
        self.client.handle_input(input)?;
        self.drain_outputs()
    }

    fn wait_state(&mut self, target: ClientState) -> anyhow::Result<()> {
        loop {
            ensure!(Instant::now() < self.deadline, "RTSP capture timed out");
            match self.client.state() {
                s if s == target => return Ok(()),
                s @ (ClientState::Failed | ClientState::Closed) => {
                    bail!("RTSP client entered {s:?} before {target:?}");
                }
                _ => self.pump_io()?,
            }
        }
    }

    fn drain_outputs(&mut self) -> anyhow::Result<()> {
        loop {
            match self.client.poll_output() {
                RtspOutput::OpenTcp { connection, target } => {
                    self.open_rtsp_socket(connection, target.host.as_ref(), target.port)?;
                }
                RtspOutput::TcpTransmit { connection, data } => {
                    self.transmit_rtsp_tcp(connection, &data)?;
                }
                RtspOutput::CloseTcp { connection } => {
                    if self
                        .socket
                        .as_ref()
                        .is_some_and(|(id, _, _)| *id == connection)
                    {
                        self.socket.take();
                    }
                }
                RtspOutput::Event(event) => self.handle_event(event)?,
                RtspOutput::Timeout(deadline) => {
                    self.retina_timeout = deadline;
                    return Ok(());
                }
                _ => bail!("unsupported RTSP core output"),
            }
        }
    }

    fn open_rtsp_socket(
        &mut self,
        connection: TcpConnectionId,
        host: &str,
        port: u16,
    ) -> anyhow::Result<()> {
        let tcp = connect_tcp_with_deadline(host, port, self.deadline)?;
        tcp.set_nodelay(true)?;
        let local = tcp.local_addr()?;
        let peer = tcp.peer_addr()?;
        let sock = if self.use_tls {
            let connector = TlsConnector::builder()
                .danger_accept_invalid_certs(self.ctx.req.tls_accept_invalid_certs)
                .build()?;
            let tls = connector
                .connect(host, tcp)
                .map_err(|err| anyhow::anyhow!("RTSPS TLS handshake failed: {err}"))?;
            RtspSocket::Tls(Box::new(tls))
        } else {
            RtspSocket::Plain(tcp)
        };
        self.socket = Some((
            connection,
            sock,
            TcpFlowKey {
                client: local,
                server: peer,
            },
        ));
        self.client.handle_input(RtspInput::TcpConnected {
            time: rtsp_now(),
            connection,
        })?;
        Ok(())
    }

    fn transmit_rtsp_tcp(
        &mut self,
        connection: TcpConnectionId,
        data: &[u8],
    ) -> anyhow::Result<()> {
        let flow = {
            let (id, sock, flow) = self.socket.as_mut().context("missing RTSP socket")?;
            ensure!(*id == connection, "unexpected RTSP connection id");
            sock.write_all(data)?;
            sock.flush()?;
            *flow
        };
        let secrets = self.ctx.req.secret_values();
        let comment = format_packet_comment(
            self.use_tls,
            &self.ctx.req.camera_id,
            if self.use_tls { "rtsps" } else { "rtsp" },
        );
        if self.ctx.req.include_raw_auth {
            self.ctx
                .write_tcp_pcap(flow, FlowDirection::ClientToServer, data, &comment)?;
        }
        self.tx_wire_buf.extend_from_slice(data);
        while let Some(msg) = try_take_rtsp_or_http_message(&mut self.tx_wire_buf) {
            let effective = if self.ctx.req.include_raw_auth {
                msg
            } else {
                let scrubbed = scrub_http_message(&msg, &secrets);
                let wire = encode_http_message(&scrubbed);
                self.ctx
                    .write_tcp_pcap(flow, FlowDirection::ClientToServer, &wire, &comment)?;
                scrubbed
            };
            self.pending_requests.push(effective);
        }
        self.client.handle_input(RtspInput::TcpWriteCompleted {
            time: rtsp_now(),
            connection,
        })?;
        Ok(())
    }

    fn handle_event(&mut self, event: RtspEvent) -> anyhow::Result<()> {
        match event {
            RtspEvent::DescribeResponse { response, body, .. } => {
                if !response.status_code.is_success() && self.client.state() == ClientState::Failed
                {
                    bail!(
                        "RTSP DESCRIBE failed: {} {}",
                        response.status_code,
                        response.reason_phrase
                    );
                }
                if !body.is_empty() {
                    let sdp_text = String::from_utf8_lossy(&body).into_owned();
                    std::fs::write(self.ctx.capture_dir.join("sdp.txt"), sdp_text.as_bytes())?;
                    self.sdp = Some(sdp_text);
                }
            }
            RtspEvent::SetupResponse { response, .. }
                if !response.status_code.is_success()
                    && self.client.state() == ClientState::Failed =>
            {
                bail!(
                    "RTSP SETUP failed: {} {}",
                    response.status_code,
                    response.reason_phrase
                );
            }
            RtspEvent::PlayResponse { response, .. }
                if !response.status_code.is_success()
                    && self.client.state() == ClientState::Failed =>
            {
                bail!(
                    "RTSP PLAY failed: {} {}",
                    response.status_code,
                    response.reason_phrase
                );
            }
            RtspEvent::UdpSetup {
                stream,
                source,
                server_port,
            } => {
                let peer_ip = self
                    .socket
                    .as_ref()
                    .context("missing RTSP TCP socket")?
                    .1
                    .peer_addr()?
                    .ip();
                let source_ip = source.unwrap_or(peer_ip);
                let pair = self
                    .udp_streams
                    .get(&stream)
                    .context("unknown RTSP UDP stream")?;
                pair.rtp.connect(SocketAddr::new(source_ip, server_port))?;
                pair.rtcp
                    .connect(SocketAddr::new(source_ip, server_port.saturating_add(1)))?;
            }
            RtspEvent::CodecItem(item) => self.record_codec_item(item)?,
            RtspEvent::TcpConnectFailed { error, .. } => bail!("RTSP TCP connect failed: {error}"),
            RtspEvent::TcpClosed { .. } => bail!("RTSP TCP closed"),
            RtspEvent::RequestTimedOut { cseq, .. } => bail!("RTSP CSeq {cseq} timed out"),
            _ => {}
        }
        Ok(())
    }

    fn record_codec_item(&mut self, item: CodecItem) -> anyhow::Result<()> {
        match item {
            CodecItem::VideoFrame(frame) => {
                let codec = match self.stream_codecs.get(&frame.stream_id()) {
                    Some(TrackCodecInfo::Video(kind)) => *kind,
                    _ => return Ok(()),
                };
                if frame.has_new_parameters()
                    && let Some(stream) = self
                        .client
                        .streams()
                        .and_then(|streams| streams.get(frame.stream_id()))
                    && let Some(ParametersRef::Video(v)) = stream.parameters()
                {
                    let params = video_params_from_retina(codec, v);
                    self.ctx.bitstream.configure_video(params);
                }
                let pts_us = frame
                    .timestamp()
                    .elapsed_duration()
                    .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX));
                self.ctx.bitstream.record_video_frame(
                    codec,
                    VideoFraming::FourByteAvcc,
                    pts_us,
                    frame.is_random_access_point(),
                    frame.data(),
                )?;
                self.frames_captured += 1;
            }
            CodecItem::AudioFrame(frame) => {
                let params = match self.stream_codecs.get(&frame.stream_id()) {
                    Some(TrackCodecInfo::Audio(params)) => *params,
                    _ => return Ok(()),
                };
                let pts_us = frame
                    .timestamp()
                    .elapsed_duration()
                    .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX));
                self.ctx
                    .bitstream
                    .record_audio_frame(params, pts_us, frame.data())?;
                self.frames_captured += 1;
            }
            _ => {}
        }
        Ok(())
    }

    fn pump_io(&mut self) -> anyhow::Result<()> {
        self.pump_udp()?;
        let timeout = self
            .deadline
            .saturating_duration_since(Instant::now())
            .min(POLL_SLICE)
            .max(Duration::from_millis(1));
        let mut buf = vec![0_u8; TCP_READ_BUF_BYTES];
        let (conn_id, flow, read_res) = {
            let (id, sock, flow) = self.socket.as_mut().context("missing RTSP socket")?;
            sock.set_read_timeout(Some(timeout))?;
            (*id, *flow, sock.read(&mut buf))
        };
        match read_res {
            Ok(0) => {
                self.apply(RtspInput::TcpClosed {
                    time: rtsp_now(),
                    connection: conn_id,
                })?;
            }
            Ok(len) => {
                let chunk = &buf[..len];
                let comment = format_packet_comment(
                    self.use_tls,
                    &self.ctx.req.camera_id,
                    if self.use_tls { "rtsps" } else { "rtsp" },
                );
                if self.ctx.req.include_raw_auth {
                    self.ctx.write_tcp_pcap(
                        flow,
                        FlowDirection::ServerToClient,
                        chunk,
                        &comment,
                    )?;
                }
                self.tap_rtsp_server_bytes(flow, &comment, chunk)?;
                self.apply(RtspInput::TcpData {
                    time: rtsp_now(),
                    connection: conn_id,
                    data: chunk,
                })?;
            }
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                if self.retina_timeout.is_some_and(|t| Instant::now() >= t) {
                    self.apply(RtspInput::Timeout { time: rtsp_now() })?;
                }
            }
            Err(err) => return Err(err.into()),
        }
        self.pump_udp()?;
        Ok(())
    }

    fn pump_udp(&mut self) -> anyhow::Result<()> {
        let mut datagrams = Vec::new();
        let mut buf = [0_u8; UDP_READ_BUF_BYTES];
        for (&stream_idx, pair) in &self.udp_streams {
            for (kind, sock) in [
                (UdpPacketKind::Rtcp, &pair.rtcp),
                (UdpPacketKind::Rtp, &pair.rtp),
            ] {
                loop {
                    match sock.recv(&mut buf) {
                        Ok(len) => {
                            let local = sock.local_addr()?;
                            let peer = sock.peer_addr().unwrap_or(local);
                            datagrams.push((
                                stream_idx,
                                kind,
                                pair.track.clone(),
                                pair.video_codec,
                                peer,
                                local,
                                buf[..len].to_vec(),
                            ));
                        }
                        Err(err)
                            if matches!(
                                err.kind(),
                                std::io::ErrorKind::WouldBlock
                                    | std::io::ErrorKind::ConnectionRefused
                            ) =>
                        {
                            break;
                        }
                        Err(err) => return Err(err.into()),
                    }
                }
            }
        }
        for (stream_idx, kind, track, video_hint, src, dst, data) in datagrams {
            let ts = unix_timestamp_us();
            let comment = format_packet_comment(false, &self.ctx.req.camera_id, "rtp-udp");
            self.ctx.write_udp_pcap(src, dst, ts, &data, &comment)?;
            if kind == UdpPacketKind::Rtp {
                self.ctx
                    .bitstream
                    .record_rtp_packet(&track, None, video_hint, ts, &data)?;
            }
            self.apply(RtspInput::UdpData {
                time: rtsp_now(),
                stream: stream_idx,
                kind,
                data: &data,
            })?;
        }
        Ok(())
    }

    fn tap_rtsp_server_bytes(
        &mut self,
        flow: TcpFlowKey,
        comment: &str,
        bytes: &[u8],
    ) -> anyhow::Result<()> {
        let secrets = self.ctx.req.secret_values();
        self.interleave_buf.extend_from_slice(bytes);
        while !self.interleave_buf.is_empty() {
            if self.interleave_buf[0] == RTSP_INTERLEAVE_MAGIC {
                if !self.try_consume_interleaved_rtp(flow, comment)? {
                    break;
                }
                continue;
            }
            if let Some(resp) = try_take_rtsp_or_http_message(&mut self.interleave_buf) {
                let effective = if self.ctx.req.include_raw_auth {
                    resp
                } else {
                    let scrubbed = scrub_http_message(&resp, &secrets);
                    let wire = encode_http_message(&scrubbed);
                    self.ctx
                        .write_tcp_pcap(flow, FlowDirection::ServerToClient, &wire, comment)?;
                    scrubbed
                };
                self.record_rtsp_exchange(effective);
                continue;
            }
            break;
        }
        Ok(())
    }

    fn try_consume_interleaved_rtp(
        &mut self,
        flow: TcpFlowKey,
        comment: &str,
    ) -> anyhow::Result<bool> {
        if self.interleave_buf.len() < 4 {
            return Ok(false);
        }
        let channel = self.interleave_buf[1];
        let payload_len =
            u16::from_be_bytes([self.interleave_buf[2], self.interleave_buf[3]]) as usize;
        let Some(total_len) = 4_usize.checked_add(payload_len) else {
            return Ok(false);
        };
        if self.interleave_buf.len() < total_len {
            return Ok(false);
        }
        let raw_interleaved: Vec<u8> = self.interleave_buf.drain(..total_len).collect();
        if !self.ctx.req.include_raw_auth {
            self.ctx.write_tcp_pcap(
                flow,
                FlowDirection::ServerToClient,
                &raw_interleaved,
                comment,
            )?;
        }
        let payload = &raw_interleaved[4..];
        if channel.is_multiple_of(2) {
            let (track, hint) = self
                .channel_tracks
                .get(&channel)
                .cloned()
                .unwrap_or_else(|| (format!("ch{channel}"), None));
            self.ctx.bitstream.record_rtp_packet(
                &track,
                Some(channel),
                hint,
                unix_timestamp_us(),
                payload,
            )?;
        }
        Ok(true)
    }

    fn record_rtsp_exchange(&mut self, resp: RawHttpMessage) {
        let req = (!self.pending_requests.is_empty()).then(|| self.pending_requests.remove(0));
        let (method, uri, request_headers, request_body) = req.map_or_else(
            || {
                (
                    "UNKNOWN".to_owned(),
                    String::new(),
                    Vec::new(),
                    String::new(),
                )
            },
            |r| {
                (
                    r.start_a,
                    r.start_b,
                    r.headers,
                    String::from_utf8_lossy(&r.body).into_owned(),
                )
            },
        );
        let delay_ms = u64::try_from(self.ctx.started.elapsed().as_millis()).unwrap_or(0);
        self.exchanges.push(RtspExchangeTranscript {
            method,
            uri,
            request_headers,
            request_body,
            status: resp.start_b.parse().unwrap_or(200),
            reason: resp.start_c,
            response_headers: resp.headers,
            response_body: String::from_utf8_lossy(&resp.body).into_owned(),
            delay_ms,
        });
    }
}

fn classify_stream_params(stream: &retina::client::Stream) -> TrackCodecInfo {
    match stream.parameters() {
        Some(ParametersRef::Video(v)) => match v.codec_params() {
            VideoParametersCodec::H264 { .. } => TrackCodecInfo::Video(VideoCodecKind::H264),
            VideoParametersCodec::H265 { .. } => TrackCodecInfo::Video(VideoCodecKind::H265),
            _ => TrackCodecInfo::Other,
        },
        Some(ParametersRef::Audio(a)) => {
            TrackCodecInfo::Audio(audio_params_from_retina(stream.encoding_name(), a))
        }
        _ => TrackCodecInfo::Other,
    }
}

fn resolve_stream_info(
    media: &str,
    encoding: &str,
    rate: u32,
    channels: Option<NonZeroU16>,
    classified: TrackCodecInfo,
    bitstream: &mut BitstreamRecorder,
) -> TrackCodecInfo {
    let resolved = match classified {
        TrackCodecInfo::Other if media == "video" => {
            if encoding.eq_ignore_ascii_case("h265") || encoding.eq_ignore_ascii_case("hevc") {
                TrackCodecInfo::Video(VideoCodecKind::H265)
            } else if encoding.eq_ignore_ascii_case("h264") {
                TrackCodecInfo::Video(VideoCodecKind::H264)
            } else {
                TrackCodecInfo::Other
            }
        }
        TrackCodecInfo::Other if media == "audio" => {
            map_audio_encoding(encoding).map_or(TrackCodecInfo::Other, |codec| {
                TrackCodecInfo::Audio(AudioStreamParams {
                    codec,
                    sample_rate_hz: rate.max(8_000),
                    channels: channels.map_or(1, |c| u8::try_from(c.get()).unwrap_or(2)),
                    aac_profile: 2,
                })
            })
        }
        other => other,
    };
    if let TrackCodecInfo::Audio(params) = resolved {
        bitstream.configure_audio(params);
    }
    resolved
}

fn video_params_from_retina(codec: VideoCodecKind, v: &VideoParameters) -> VideoStreamParams {
    let (w, h) = v.pixel_dimensions();
    let (vps, sps, pps) = match v.codec_params() {
        VideoParametersCodec::H264 { sps, pps } => (None, Some(sps.to_vec()), Some(pps.to_vec())),
        VideoParametersCodec::H265 { vps, sps, pps } => {
            (Some(vps.to_vec()), Some(sps.to_vec()), Some(pps.to_vec()))
        }
        _ => (None, None, None),
    };
    VideoStreamParams {
        codec,
        width: u16::try_from(w).unwrap_or(1280),
        height: u16::try_from(h).unwrap_or(720),
        sps,
        pps,
        vps,
    }
}

fn audio_params_from_retina(encoding: &str, a: &AudioParameters) -> AudioStreamParams {
    let codec = map_audio_encoding(encoding).unwrap_or(AudioElementaryCodec::Aac);
    AudioStreamParams {
        codec,
        sample_rate_hz: a.clock_rate().max(8_000),
        channels: u8::try_from(a.channels().get()).unwrap_or(2),
        aac_profile: 2,
    }
}

const fn map_audio_encoding(encoding: &str) -> Option<AudioElementaryCodec> {
    if encoding.eq_ignore_ascii_case("mpeg4-generic") || encoding.eq_ignore_ascii_case("aac") {
        Some(AudioElementaryCodec::Aac)
    } else if encoding.eq_ignore_ascii_case("pcmu") {
        Some(AudioElementaryCodec::Pcmu)
    } else if encoding.eq_ignore_ascii_case("pcma") {
        Some(AudioElementaryCodec::Pcma)
    } else if encoding.eq_ignore_ascii_case("opus") {
        Some(AudioElementaryCodec::Opus)
    } else {
        None
    }
}

struct HttpTapState {
    camera_id: String,
    proto_label: &'static str,
    include_raw_auth: bool,
    secrets: Vec<String>,
    started: Instant,
    tls_decrypted: bool,
    next_ephemeral_port: u16,
    last_tx_flow: Option<TcpFlowKey>,
    last_rx_flow: Option<TcpFlowKey>,
    tx_buf: Vec<u8>,
    rx_buf: Vec<u8>,
    pending_requests: Vec<RawHttpMessage>,
    exchanges: Vec<HttpExchangeTranscript>,
    pcap: Option<PcapngWriter<BufWriter<File>>>,
}

impl Drop for HttpTapState {
    fn drop(&mut self) {
        for secret in &mut self.secrets {
            secret.zeroize();
        }
    }
}

#[derive(Clone, Debug)]
struct TapConnector {
    state: Arc<Mutex<HttpTapState>>,
}

impl std::fmt::Debug for HttpTapState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpTapState")
            .field("proto_label", &self.proto_label)
            .field("exchanges", &self.exchanges.len())
            .finish_non_exhaustive()
    }
}

impl<Inner: Transport> Connector<Inner> for TapConnector {
    type Out = TapTransport<Inner>;

    fn connect(
        &self,
        details: &ConnectionDetails,
        chained: Option<Inner>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        let Some(inner) = chained else {
            return Ok(None);
        };
        let remote = details
            .addrs
            .first()
            .copied()
            .unwrap_or_else(|| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 80));
        let local_ip = if remote.is_ipv6() {
            IpAddr::V6(Ipv6Addr::LOCALHOST)
        } else {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        };
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let port = guard.alloc_ephemeral_port();
        if inner.is_tls() {
            guard.tls_decrypted = true;
        }
        Ok(Some(TapTransport {
            inner,
            state: Arc::clone(&self.state),
            flow: TcpFlowKey {
                client: SocketAddr::new(local_ip, port),
                server: remote,
            },
        }))
    }
}

#[derive(Debug)]
struct TapTransport<Inner> {
    inner: Inner,
    state: Arc<Mutex<HttpTapState>>,
    flow: TcpFlowKey,
}

impl<Inner> Drop for TapTransport<Inner> {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.state.lock() {
            guard.close_flow(self.flow);
        }
    }
}

impl<Inner: Transport> Transport for TapTransport<Inner> {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.inner.buffers()
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        let is_tls = self.inner.is_tls();
        let bytes = self.inner.buffers().output()[..amount].to_vec();
        if let Ok(mut guard) = self.state.lock() {
            let _ = guard.record_tx(self.flow, is_tls, &bytes);
        }
        self.inner.transmit_output(amount, timeout)
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        let before_len = self.inner.buffers().input().len();
        let progressed = self.inner.await_input(timeout)?;
        let is_tls = self.inner.is_tls();
        let new_bytes = {
            let after = self.inner.buffers().input();
            if after.len() > before_len {
                Some(after[before_len..].to_vec())
            } else {
                None
            }
        };
        if let Some(bytes) = new_bytes
            && let Ok(mut guard) = self.state.lock()
        {
            let _ = guard.record_rx(self.flow, is_tls, &bytes);
        }
        Ok(progressed)
    }

    fn is_open(&mut self) -> bool {
        self.inner.is_open()
    }

    fn is_tls(&self) -> bool {
        self.inner.is_tls()
    }
}

impl HttpTapState {
    const fn alloc_ephemeral_port(&mut self) -> u16 {
        let port = self.next_ephemeral_port;
        self.next_ephemeral_port = if port >= 65_534 { 49_152 } else { port + 1 };
        port
    }

    fn close_flow(&mut self, flow: TcpFlowKey) {
        if let Some(pcap) = self.pcap.as_mut() {
            let _ = pcap.close_tcp_flow(
                flow,
                FlowDirection::ClientToServer,
                unix_timestamp_us(),
                None,
            );
        }
    }

    fn write_pcap(
        &mut self,
        flow: TcpFlowKey,
        dir: FlowDirection,
        tls_decrypted: bool,
        wire: &[u8],
    ) {
        let comment = format_packet_comment(tls_decrypted, &self.camera_id, self.proto_label);
        if let Some(pcap) = self.pcap.as_mut() {
            let _ = pcap.write_tcp_payload(flow, dir, unix_timestamp_us(), wire, Some(&comment));
        }
    }

    fn record_tx(
        &mut self,
        flow: TcpFlowKey,
        tls_decrypted: bool,
        bytes: &[u8],
    ) -> anyhow::Result<()> {
        self.last_tx_flow = Some(flow);
        if self.include_raw_auth {
            self.write_pcap(flow, FlowDirection::ClientToServer, tls_decrypted, bytes);
        }
        self.tx_buf.extend_from_slice(bytes);
        while let Some(msg) = try_take_rtsp_or_http_message(&mut self.tx_buf) {
            let effective = if self.include_raw_auth {
                msg
            } else {
                let scrubbed = scrub_http_message(&msg, &self.secrets);
                let wire = encode_http_message(&scrubbed);
                self.write_pcap(flow, FlowDirection::ClientToServer, tls_decrypted, &wire);
                scrubbed
            };
            self.pending_requests.push(effective);
        }
        Ok(())
    }

    fn record_rx(
        &mut self,
        flow: TcpFlowKey,
        tls_decrypted: bool,
        bytes: &[u8],
    ) -> anyhow::Result<()> {
        self.last_rx_flow = Some(flow);
        if self.include_raw_auth {
            self.write_pcap(flow, FlowDirection::ServerToClient, tls_decrypted, bytes);
        }
        self.rx_buf.extend_from_slice(bytes);
        while let Some(resp) = try_take_rtsp_or_http_message(&mut self.rx_buf) {
            let effective = if self.include_raw_auth {
                resp
            } else {
                let scrubbed = scrub_http_message(&resp, &self.secrets);
                let wire = encode_http_message(&scrubbed);
                self.write_pcap(flow, FlowDirection::ServerToClient, tls_decrypted, &wire);
                scrubbed
            };
            let req = (!self.pending_requests.is_empty()).then(|| self.pending_requests.remove(0));
            let delay_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(0);
            self.exchanges
                .push(build_http_exchange(req, effective, delay_ms));
        }
        Ok(())
    }

    fn flush_trailing_unparsed(&mut self) {
        if self.include_raw_auth {
            return;
        }
        let tls = self.tls_decrypted;
        if !self.tx_buf.is_empty()
            && let Some(flow) = self.last_tx_flow
        {
            let raw = std::mem::take(&mut self.tx_buf);
            let scrubbed = redact_wire_bytes_if_needed(&raw, false, &self.secrets);
            self.write_pcap(flow, FlowDirection::ClientToServer, tls, &scrubbed);
        }
        if !self.rx_buf.is_empty()
            && let Some(flow) = self.last_rx_flow
        {
            let raw = std::mem::take(&mut self.rx_buf);
            let scrubbed = redact_wire_bytes_if_needed(&raw, false, &self.secrets);
            self.write_pcap(flow, FlowDirection::ServerToClient, tls, &scrubbed);
        }
    }
}

fn build_tapped_http_agent(
    ctx: &mut CaptureContext<'_>,
    proto_label: &'static str,
) -> anyhow::Result<(Agent, Arc<Mutex<HttpTapState>>)> {
    let pcap = ctx.pcap.take();
    let state = Arc::new(Mutex::new(HttpTapState {
        camera_id: ctx.req.camera_id.clone(),
        proto_label,
        include_raw_auth: ctx.req.include_raw_auth,
        secrets: ctx.req.secret_values(),
        started: ctx.started,
        tls_decrypted: false,
        next_ephemeral_port: 49_152,
        last_tx_flow: None,
        last_rx_flow: None,
        tx_buf: Vec::new(),
        rx_buf: Vec::new(),
        pending_requests: Vec::new(),
        exchanges: Vec::new(),
        pcap,
    }));
    let timeout = Duration::from_millis(ctx.req.duration_max_ms);
    let agent = build_tapped_http_agent_reuse(&state, timeout, ctx.req.tls_accept_invalid_certs)?;
    Ok((agent, state))
}

fn build_tapped_http_agent_reuse(
    state: &Arc<Mutex<HttpTapState>>,
    timeout: Duration,
    tls_accept_invalid_certs: bool,
) -> anyhow::Result<Agent> {
    let tls_config = TlsConfig::builder()
        .provider(TlsProvider::NativeTls)
        .root_certs(RootCerts::PlatformVerifier)
        .disable_verification(tls_accept_invalid_certs)
        .build();
    let config = Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .tls_config(tls_config)
        .build();
    let connector =
        ().chain(TcpConnector::default())
            .chain(NativeTlsConnector::default())
            .chain(TapConnector {
                state: Arc::clone(state),
            });
    Ok(Agent::with_parts(
        config,
        connector,
        DefaultResolver::default(),
    ))
}

impl CaptureContext<'_> {
    fn finish_http_tap(
        &mut self,
        state: Arc<Mutex<HttpTapState>>,
    ) -> anyhow::Result<Vec<HttpExchangeTranscript>> {
        let mut guard = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.flush_trailing_unparsed();
        self.tls_decrypted = guard.tls_decrypted;
        if let Some(mut pcap) = guard.pcap.take() {
            pcap.flush()?;
            self.pcap = Some(pcap);
        }
        Ok(std::mem::take(&mut guard.exchanges))
    }
}

fn capture_onvif(
    ctx: &mut CaptureContext<'_>,
    url: &Url,
) -> anyhow::Result<Vec<HttpExchangeTranscript>> {
    let (agent, tap) = build_tapped_http_agent(ctx, "onvif")?;
    let creds = (!ctx.req.username.is_empty()).then(|| OnvifCredentials {
        username: ctx.req.username.clone(),
        password: ctx.req.password.clone(),
    });
    let tls_ver = if ctx.req.tls_accept_invalid_certs {
        TlsVerification::AcceptInvalidCertificates
    } else {
        TlsVerification::Strict
    };
    let client = OnvifClientBuilder::new(url)
        .agent(agent)
        .credentials(creds.clone())
        .auth_type(OnvifAuthType::Any)
        .tls_verification(tls_ver)
        .follow_redirects(false)
        .timeout(Duration::from_millis(ctx.req.duration_max_ms))
        .build();
    let _ = schema::devicemgmt::get_device_information(
        &client,
        &schema::devicemgmt::GetDeviceInformation {},
    );
    if ctx.req.trigger_probe {
        probe_onvif_media(&client, &tap, ctx, creds, tls_ver)?;
    }
    ctx.finish_http_tap(tap)
}

fn probe_onvif_media(
    client: &onvif::soap::client::Client,
    tap: &Arc<Mutex<HttpTapState>>,
    ctx: &CaptureContext<'_>,
    creds: Option<OnvifCredentials>,
    tls_ver: TlsVerification,
) -> anyhow::Result<()> {
    let remaining = Duration::from_millis(ctx.req.duration_max_ms)
        .saturating_sub(ctx.started.elapsed())
        .max(Duration::from_millis(100));
    let Ok(resp) = schema::devicemgmt::get_services(
        client,
        &schema::devicemgmt::GetServices {
            include_capability: true,
        },
    ) else {
        return Ok(());
    };
    let Some(media_url) = resp
        .service
        .iter()
        .find(|s| s.namespace.contains("media/wsdl"))
        .and_then(|s| Url::parse(&s.x_addr).ok())
    else {
        return Ok(());
    };
    let media_agent =
        build_tapped_http_agent_reuse(tap, remaining, ctx.req.tls_accept_invalid_certs)?;
    let media_client = OnvifClientBuilder::new(&media_url)
        .agent(media_agent)
        .credentials(creds)
        .auth_type(OnvifAuthType::Any)
        .tls_verification(tls_ver)
        .follow_redirects(false)
        .timeout(remaining)
        .build();
    if let Ok(profiles) = schema::media::get_profiles(&media_client, &schema::media::GetProfiles {})
        && let Some(first) = profiles.profiles.first()
    {
        let _ = schema::media::get_stream_uri(
            &media_client,
            &schema::media::GetStreamUri {
                stream_setup: schema::onvif::StreamSetup {
                    stream: schema::onvif::StreamType::RtpUnicast,
                    transport: schema::onvif::Transport {
                        protocol: schema::onvif::TransportProtocol::Rtsp,
                        tunnel: vec![],
                    },
                },
                profile_token: schema::onvif::ReferenceToken(first.token.0.clone()),
            },
        );
    }
    Ok(())
}

fn capture_isapi(
    ctx: &mut CaptureContext<'_>,
    url: &Url,
) -> anyhow::Result<Vec<HttpExchangeTranscript>> {
    let (agent, tap) = build_tapped_http_agent(ctx, "isapi")?;
    let origin = url.origin().ascii_serialization();
    let creds = ::isapi::Credentials::new(&ctx.req.username, &ctx.req.password);
    let mut session = ::isapi::Session::new(creds)?;
    let paths = if ctx.req.trigger_probe {
        vec!["/ISAPI/System/deviceInfo", "/ISAPI/System/status"]
    } else {
        vec!["/ISAPI/System/deviceInfo"]
    };
    for path in paths {
        let req = ::isapi::Request::get(path)?;
        execute_tapped_isapi_request(&agent, &origin, &mut session, &req)?;
    }
    drop(agent);
    ctx.finish_http_tap(tap)
}

fn execute_tapped_isapi_request(
    agent: &Agent,
    origin: &str,
    session: &mut ::isapi::Session,
    req: &::isapi::Request,
) -> anyhow::Result<Vec<u8>> {
    for attempt in 0..3 {
        let cnonce = format!("{:032x}", rand::random::<u128>());
        let auth = session.authorization(req, &cnonce)?;
        let mut builder = ureq::http::Request::builder()
            .method(req.method().as_str())
            .uri(format!("{origin}{}", req.resource()))
            .header("accept-encoding", "identity");
        if let Some(ct) = req.content_type() {
            builder = builder.header("content-type", ct);
        }
        if let Some(auth_hdr) = auth.as_ref() {
            builder = builder.header("authorization", auth_hdr.as_str());
        }
        let wire = builder.body(req.body())?;
        let response = ureq::RequestExt::with_agent(wire, agent).run()?;
        if response.status().as_u16() == 401 {
            let challenge = response
                .headers()
                .get_all("www-authenticate")
                .iter()
                .filter_map(|v| v.to_str().ok())
                .find(|v| v.to_ascii_lowercase().starts_with("digest "))
                .context("ISAPI 401 missing Digest challenge")?;
            let stale = session.handle_challenge(challenge)?;
            if (auth.is_some() && !stale) || attempt == 2 {
                bail!("ISAPI authentication rejected");
            }
            continue;
        }
        let bytes = response
            .into_body()
            .into_with_config()
            .limit(HTTP_RESPONSE_LIMIT_BYTES)
            .read_to_vec()?;
        return Ok(bytes);
    }
    bail!("ISAPI authentication exhausted retries")
}

fn capture_reolink_http(
    ctx: &mut CaptureContext<'_>,
    url: &Url,
) -> anyhow::Result<Vec<HttpExchangeTranscript>> {
    let (agent, tap) = build_tapped_http_agent(ctx, "reolink_http")?;
    let origin = url.origin().ascii_serialization();
    let login_body = serde_json::json!([{
        "cmd": "Login",
        "action": 0,
        "param": { "User": { "userName": ctx.req.username, "password": ctx.req.password } }
    }]);
    let login_rsp: serde_json::Value =
        post_reolink_cmd(&agent, &origin, "Login", None, login_body)?;
    let token = login_rsp
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|item| item.get("value"))
        .and_then(|val| val.get("Token"))
        .and_then(|tok| tok.get("name"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let cmds = if ctx.req.trigger_probe {
        vec!["GetDevInfo", "GetEnc", "GetAbility", "Logout"]
    } else {
        vec!["GetDevInfo", "Logout"]
    };
    for cmd in cmds {
        let body = serde_json::json!([{ "cmd": cmd, "action": 0, "param": {} }]);
        let _ = post_reolink_cmd(&agent, &origin, cmd, token.as_deref(), body);
    }
    drop(agent);
    ctx.finish_http_tap(tap)
}

fn post_reolink_cmd(
    agent: &Agent,
    origin: &str,
    cmd: &str,
    token: Option<&str>,
    body: serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let mut url = Url::parse(&format!("{origin}/cgi-bin/api.cgi"))?;
    url.query_pairs_mut().append_pair("cmd", cmd);
    if let Some(tok) = token {
        url.query_pairs_mut().append_pair("token", tok);
    }
    let rsp = agent
        .post(url.as_str())
        .header("Content-Type", "application/json")
        .header("Accept-Encoding", "identity")
        .send(body.to_string())?;
    let bytes = rsp
        .into_body()
        .into_with_config()
        .limit(HTTP_RESPONSE_LIMIT_BYTES)
        .read_to_vec()?;
    Ok(serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

fn capture_reo_proto(
    ctx: &mut CaptureContext<'_>,
    url: &Url,
) -> anyhow::Result<Vec<ReoFrameTranscript>> {
    let host = url.host_str().context("reo_proto URL missing host")?;
    let port = url.port().unwrap_or(crate::cameras::BAICHUAN_PORT);
    let deadline = Instant::now() + Duration::from_millis(ctx.req.duration_max_ms);
    let mut tcp = connect_tcp_with_deadline(host, port, deadline)?;
    tcp.set_nodelay(true)?;
    let flow = TcpFlowKey {
        client: tcp.local_addr()?,
        server: tcp.peer_addr()?,
    };
    let mut session = BcSession::new(
        BcSessionConfig {
            role: BcRole::Client,
            keepalive_channel: 0,
            ..BcSessionConfig::default_client()
        },
        Instant::now(),
    );
    session.handle_input(BcInput::Command(BcCommand::Login(LoginParams::new(
        &ctx.req.username,
        &ctx.req.password,
        EncryptionMode::BcEncrypt,
    ))))?;
    let mut state = ReoCaptureState::default();
    let mut rx_buf = vec![0_u8; TCP_READ_BUF_BYTES];

    while Instant::now() < deadline && state.frames_captured < ctx.req.frames_max {
        drain_reo_outputs(ctx, &mut session, &mut tcp, flow, &mut state)?;
        let timeout = deadline
            .saturating_duration_since(Instant::now())
            .min(POLL_SLICE)
            .max(Duration::from_millis(1));
        tcp.set_read_timeout(Some(timeout))?;
        match tcp.read(&mut rx_buf) {
            Ok(0) => break,
            Ok(len) => handle_reo_tcp_chunk(ctx, &mut session, flow, &rx_buf[..len], &mut state)?,
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                let _ = session.handle_input(BcInput::Timeout(Instant::now()));
            }
            Err(err) => return Err(err.into()),
        }
    }
    ctx.close_tcp_pcap(flow);
    Ok(state.frames)
}

#[derive(Default)]
struct ReoCaptureState {
    frames: Vec<ReoFrameTranscript>,
    rx_read_buf: reo_proto::ReadBuffer,
    tx_read_buf: reo_proto::ReadBuffer,
    stream_started: bool,
    frames_captured: u64,
}

fn handle_reo_tcp_chunk(
    ctx: &mut CaptureContext<'_>,
    session: &mut BcSession,
    flow: TcpFlowKey,
    chunk: &[u8],
    state: &mut ReoCaptureState,
) -> anyhow::Result<()> {
    let comment = format_packet_comment(false, &ctx.req.camera_id, "reo_proto");
    let secrets = ctx.req.secret_values();
    if ctx.req.include_raw_auth {
        ctx.write_tcp_pcap(flow, FlowDirection::ServerToClient, chunk, &comment)?;
    }
    state.rx_read_buf.extend(chunk);
    record_buffered_baichuan_frames(
        ctx,
        &mut state.rx_read_buf,
        flow,
        FlowDirection::ServerToClient,
        &comment,
        &secrets,
        &mut state.frames,
    )?;
    session.handle_input(BcInput::TcpData(Instant::now(), chunk))?;
    Ok(())
}

fn drain_reo_outputs(
    ctx: &mut CaptureContext<'_>,
    session: &mut BcSession,
    tcp: &mut TcpStream,
    flow: TcpFlowKey,
    state: &mut ReoCaptureState,
) -> anyhow::Result<()> {
    let mut media_buf = vec![0_u8; 512 * 1024];
    let secrets = ctx.req.secret_values();
    loop {
        match session.poll_output(&mut media_buf) {
            Ok(BcOutput::TcpSend { data }) => {
                tcp.write_all(data)?;
                tcp.flush()?;
                let comment = format_packet_comment(false, &ctx.req.camera_id, "reo_proto");
                if ctx.req.include_raw_auth {
                    ctx.write_tcp_pcap(flow, FlowDirection::ClientToServer, data, &comment)?;
                }
                state.tx_read_buf.extend(data);
                record_buffered_baichuan_frames(
                    ctx,
                    &mut state.tx_read_buf,
                    flow,
                    FlowDirection::ClientToServer,
                    &comment,
                    &secrets,
                    &mut state.frames,
                )?;
            }
            Ok(BcOutput::Event(event)) => handle_reo_event(ctx, session, event, state)?,
            Ok(BcOutput::Timeout(_)) => return Ok(()),
            Err(BcError::BufferTooSmall { needed, .. }) => media_buf.resize(needed, 0),
            Err(err) => return Err(err.into()),
        }
    }
}

fn handle_reo_event(
    ctx: &mut CaptureContext<'_>,
    session: &mut BcSession,
    event: BcEvent<'_>,
    state: &mut ReoCaptureState,
) -> anyhow::Result<()> {
    match event {
        BcEvent::LoggedIn(_) if !state.stream_started => {
            state.stream_started = true;
            session.handle_input(BcInput::Command(BcCommand::SubscribeStream(
                StreamSubscription {
                    channel: 0,
                    stream_type: StreamType::Main,
                    expected_width: 1280,
                    expected_height: 720,
                },
            )))?;
        }
        BcEvent::VideoFrame {
            codec,
            is_keyframe,
            data,
            timestamp,
            ..
        } => {
            let kind = match codec {
                BcVideoCodec::H264 => VideoCodecKind::H264,
                BcVideoCodec::H265 => VideoCodecKind::H265,
            };
            let pts_us = u64::try_from(timestamp.as_micros()).unwrap_or(u64::MAX);
            ctx.bitstream.record_video_frame(
                kind,
                VideoFraming::AnnexB,
                pts_us,
                is_keyframe,
                data,
            )?;
            state.frames_captured += 1;
        }
        BcEvent::AudioFrame {
            codec,
            data,
            duration,
            ..
        } => {
            if let Some(elem_codec) = map_bc_audio_codec(codec) {
                let params = AudioStreamParams {
                    codec: elem_codec,
                    sample_rate_hz: 16_000,
                    channels: 1,
                    aac_profile: 2,
                };
                let pts_us =
                    u64::try_from(duration.as_micros()).unwrap_or(0) * (state.frames_captured + 1);
                ctx.bitstream.record_audio_frame(params, pts_us, data)?;
                state.frames_captured += 1;
            }
        }
        BcEvent::LoginFailed(status) => bail!("Baichuan login failed (status {status})"),
        _ => {}
    }
    Ok(())
}

const fn map_bc_audio_codec(codec: BcAudioCodec) -> Option<AudioElementaryCodec> {
    match codec {
        BcAudioCodec::Aac => Some(AudioElementaryCodec::Aac),
        BcAudioCodec::G711Ulaw => Some(AudioElementaryCodec::Pcmu),
        BcAudioCodec::G711Alaw => Some(AudioElementaryCodec::Pcma),
        _ => None,
    }
}

fn record_buffered_baichuan_frames(
    ctx: &mut CaptureContext<'_>,
    reader: &mut reo_proto::ReadBuffer,
    flow: TcpFlowKey,
    dir: FlowDirection,
    comment: &str,
    secrets: &[String],
    out: &mut Vec<ReoFrameTranscript>,
) -> anyhow::Result<()> {
    let direction_label = match dir {
        FlowDirection::ClientToServer => "client_to_server",
        FlowDirection::ServerToClient => "server_to_client",
    };
    while let Ok(Some(msg)) = reader.try_parse_message() {
        let is_login = msg.header.msg_id == reo_proto::COMMAND_LOGIN;
        if !ctx.req.include_raw_auth {
            let wire = serialize_baichuan_packet_for_pcap(&msg.header, &msg.body, is_login);
            ctx.write_tcp_pcap(flow, dir, &wire, comment)?;
        }
        if out.len() >= 128 {
            continue;
        }
        let xml = (!msg.body.is_empty() && msg.body.starts_with(b"<")).then(|| {
            let text = String::from_utf8_lossy(&msg.body).into_owned();
            if ctx.req.include_raw_auth {
                text
            } else {
                repro::scrub_wire_text(&text, secrets)
            }
        });
        let b64 = (xml.is_none() && (ctx.req.include_raw_auth || !is_login))
            .then(|| BASE64.encode(&msg.body));
        out.push(ReoFrameTranscript {
            direction: direction_label.to_owned(),
            msg_id: msg.header.msg_id,
            channel_id: msg.header.channel_id(),
            stream_type: msg.header.stream_type_id(),
            payload_xml: xml,
            payload_base64: b64,
            delay_ms: u64::try_from(ctx.started.elapsed().as_millis()).unwrap_or(0),
        });
    }
    Ok(())
}

fn serialize_baichuan_packet_for_pcap(
    header: &PacketHeader,
    body: &[u8],
    redact_body: bool,
) -> Vec<u8> {
    let payload: &[u8] = if redact_body { b"[REDACTED]" } else { body };
    let mut hdr = *header;
    hdr.body_len = u32::try_from(payload.len()).unwrap_or(0);
    if redact_body {
        hdr.encryption_offset = 0;
    }
    let mut hdr_buf = [0_u8; reo_proto::magic::HEADER_LEN_EXTENDED];
    let hlen = hdr.serialize(&mut hdr_buf);
    let mut out = Vec::with_capacity(hlen + payload.len());
    out.extend_from_slice(&hdr_buf[..hlen]);
    out.extend_from_slice(payload);
    out
}

#[derive(Clone)]
struct RawHttpMessage {
    start_a: String,
    start_b: String,
    start_c: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn try_take_rtsp_or_http_message(buf: &mut Vec<u8>) -> Option<RawHttpMessage> {
    let header_end = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&buf[..header_end]).ok()?;
    let mut lines = head.split("\r\n");
    let start = lines.next()?;
    let mut parts = start.splitn(3, ' ');
    let start_a = parts.next()?.to_owned();
    let start_b = parts.next()?.to_owned();
    let start_c = parts.next().unwrap_or("").to_owned();
    let mut headers = Vec::new();
    let mut content_len = 0_usize;
    let mut chunked = false;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let name = k.trim().to_owned();
            let val = v.trim().to_owned();
            if name.eq_ignore_ascii_case("content-length") {
                content_len = val.parse().unwrap_or(0);
            }
            if name.eq_ignore_ascii_case("transfer-encoding")
                && val.to_ascii_lowercase().contains("chunked")
            {
                chunked = true;
            }
            headers.push((name, val));
        }
    }
    let body_start = header_end.checked_add(4)?;
    if chunked {
        let (decoded, total_consumed) = decode_chunked_body(&buf[body_start..])?;
        let drain_end = body_start.checked_add(total_consumed)?;
        buf.drain(..drain_end);
        return Some(RawHttpMessage {
            start_a,
            start_b,
            start_c,
            headers,
            body: decoded,
        });
    }
    if content_len > HTTP_RESPONSE_LIMIT_BYTES as usize {
        return None;
    }
    let total_len = body_start.checked_add(content_len)?;
    if buf.len() < total_len {
        return None;
    }
    let body = buf[body_start..total_len].to_vec();
    buf.drain(..total_len);
    Some(RawHttpMessage {
        start_a,
        start_b,
        start_c,
        headers,
        body,
    })
}

fn decode_chunked_body(data: &[u8]) -> Option<(Vec<u8>, usize)> {
    let mut pos = 0_usize;
    let mut out = Vec::new();
    let limit = HTTP_RESPONSE_LIMIT_BYTES as usize;
    loop {
        let line_end = data.get(pos..)?.windows(2).position(|w| w == b"\r\n")?;
        let size_end = pos.checked_add(line_end)?;
        let size_str = std::str::from_utf8(data.get(pos..size_end)?)
            .ok()?
            .split(';')
            .next()?
            .trim();
        let size = usize::from_str_radix(size_str, 16).ok()?;
        pos = size_end.checked_add(2)?;
        if size == 0 {
            let end = pos.checked_add(2)?;
            if data.len() < end {
                return None;
            }
            return Some((out, end));
        }
        if size > limit || out.len().checked_add(size)? > limit {
            return None;
        }
        let chunk_end = pos.checked_add(size)?;
        let next_pos = chunk_end.checked_add(2)?;
        if data.len() < next_pos {
            return None;
        }
        out.extend_from_slice(&data[pos..chunk_end]);
        pos = next_pos;
    }
}

fn scrub_http_message(msg: &RawHttpMessage, secrets: &[String]) -> RawHttpMessage {
    let body = std::str::from_utf8(&msg.body).map_or_else(
        |_| msg.body.clone(),
        |text| repro::scrub_wire_text(text, secrets).into_bytes(),
    );
    let mut headers = Vec::with_capacity(msg.headers.len());
    for (k, v) in &msg.headers {
        let val = if repro::is_sensitive_header_name(k) {
            "[REDACTED]".to_owned()
        } else if k.eq_ignore_ascii_case("content-length") {
            body.len().to_string()
        } else {
            repro::scrub_wire_text(v, secrets)
        };
        headers.push((k.clone(), val));
    }
    RawHttpMessage {
        start_a: msg.start_a.clone(),
        start_b: repro::scrub_wire_text(&msg.start_b, secrets),
        start_c: msg.start_c.clone(),
        headers,
        body,
    }
}

fn encode_http_message(msg: &RawHttpMessage) -> Vec<u8> {
    let mut out = Vec::new();
    let start = if msg.start_c.is_empty() {
        format!("{} {}\r\n", msg.start_a, msg.start_b)
    } else {
        format!("{} {} {}\r\n", msg.start_a, msg.start_b, msg.start_c)
    };
    out.extend_from_slice(start.as_bytes());
    for (k, v) in &msg.headers {
        out.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(&msg.body);
    out
}

fn build_http_exchange(
    req: Option<RawHttpMessage>,
    resp: RawHttpMessage,
    delay_ms: u64,
) -> HttpExchangeTranscript {
    let (method, path, request_headers, request_body_text, soap_action) = match req {
        Some(r) => {
            let soap = r
                .headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("soapaction"))
                .map(|(_, v)| v.clone());
            let body_text =
                (!r.body.is_empty()).then(|| String::from_utf8_lossy(&r.body).into_owned());
            (r.start_a, r.start_b, r.headers, body_text, soap)
        }
        None => ("GET".to_owned(), "/".to_owned(), Vec::new(), None, None),
    };
    let (response_body_text, response_body_base64) = match std::str::from_utf8(&resp.body) {
        Ok(text) => (Some(text.to_owned()), None),
        Err(_) => (None, Some(BASE64.encode(&resp.body))),
    };
    HttpExchangeTranscript {
        method,
        path,
        soap_action,
        request_headers,
        request_body_text,
        status: resp.start_b.parse().unwrap_or(200),
        status_reason: resp.start_c,
        response_headers: resp.headers,
        response_body_text,
        response_body_base64,
        delay_ms,
    }
}

fn redact_wire_bytes_if_needed(
    bytes: &[u8],
    include_raw_auth: bool,
    secrets: &[String],
) -> Vec<u8> {
    if include_raw_auth {
        return bytes.to_vec();
    }
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    for segment in text.split_inclusive("\r\n") {
        if let Some((name, _)) = segment.split_once(':')
            && repro::is_sensitive_header_name(name.trim())
        {
            out.push_str(name);
            out.push_str(": [REDACTED]\r\n");
        } else {
            out.push_str(&repro::scrub_wire_text(segment, secrets));
        }
    }
    out.into_bytes()
}

fn connect_tcp_with_deadline(
    host: &str,
    port: u16,
    deadline: Instant,
) -> anyhow::Result<TcpStream> {
    let mut last_err = None;
    for addr in (host, port).to_socket_addrs()? {
        let timeout = deadline
            .checked_duration_since(Instant::now())
            .context("connect deadline expired")?
            .min(CONNECT_TIMEOUT);
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(stream) => return Ok(stream),
            Err(err) => last_err = Some(err),
        }
    }
    match last_err {
        Some(err) => Err(err.into()),
        None => bail!("no addresses resolved for {host}:{port}"),
    }
}

fn remaining_transcript_budget(capture_dir: &Path, max_capture_bytes: u64) -> usize {
    let used = std::fs::read_dir(capture_dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|e| e.metadata().ok())
        .filter(std::fs::Metadata::is_file)
        .map(|m| m.len())
        .sum::<u64>();
    let budget = max_capture_bytes.saturating_sub(used.saturating_add(4_096));
    usize::try_from(budget).unwrap_or(usize::MAX).max(1_024)
}

fn save_transcript_json_bounded(
    capture_dir: &Path,
    transcript: &mut ProtocolTranscript,
    max_bytes: usize,
) -> anyhow::Result<()> {
    let mut json = serde_json::to_vec_pretty(transcript)?;
    while json.len() > max_bytes {
        let truncated = match transcript.rtsp.as_mut() {
            Some(rtsp) if !rtsp.rtp_packets.is_empty() => {
                let keep = rtsp.rtp_packets.len() / 2;
                rtsp.rtp_packets.truncate(keep);
                true
            }
            _ => false,
        };
        if !truncated {
            break;
        }
        json = serde_json::to_vec_pretty(transcript)?;
    }
    std::fs::write(capture_dir.join("protocol_transcript.json"), json)?;
    Ok(())
}

fn collect_dir_filenames(dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && let Some(name) = entry.file_name().to_str()
        {
            files.push(name.to_owned());
        }
    }
    files.sort();
    Ok(files)
}

const fn transport_label(transport: CameraTransport) -> &'static str {
    match transport {
        CameraTransport::Tcp => "tcp",
        CameraTransport::Udp => "udp",
    }
}

fn rtsp_now() -> RtspTime {
    RtspTime {
        monotonic: Instant::now(),
        wall: SystemTime::now(),
    }
}

fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera_debug::store::StoreLimits;
    use reo_proto::{
        COMMAND_LOGIN, COMMAND_STREAM, CameraIdentity, MAX_XML_BODY, auth, encryption,
        magic::{BC_CLASS_LEGACY, BC_CLASS_MODERN_EXT, HEADER_LEN_EXTENDED, make_status},
        media::{MEDIA_MAGIC_IFRAME_BASE, MEDIA_MAGIC_INFO_V1},
    };
    use retina::server::RtspServer;
    use std::net::TcpListener;
    use test_hikvision::{FakeHikvision, Reply as HikvisionReply};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("keeppeek-protocols-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fixture_mp4(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("crates/test-camera/testdata")
            .join(name)
    }

    #[test]
    fn captures_rtsp_tcp_and_udp_and_compares_transports_against_retina_server() {
        let mp4 = fixture_mp4("cc-4k-640x360-h264.mp4");
        let server = RtspServer::from_mp4(&mp4).unwrap();
        let root = TempDir::new();
        let store = CaptureStore::new(root.path(), StoreLimits::default()).unwrap();

        let mut req = CaptureRequest::new(
            "rtsp-cmp",
            "cam-rtsp",
            CaptureProtocol::Rtsp,
            server.url().as_str(),
        );
        req.username = "operator".to_owned();
        req.password = "swordfish".to_owned();
        req.duration_max_ms = 2_000;
        req.frames_max = 10;
        let report =
            compare_transports(&store, &req, &[CameraTransport::Tcp, CameraTransport::Udp])
                .unwrap();
        assert_eq!(report.entries.len(), 2);
        assert!(report.entries[0].success);
        assert_eq!(report.recommended_transport.as_deref(), Some("tcp"));

        let tcp_dir = store.capture_dir(&report.entries[0].capture_id).unwrap();
        assert!(tcp_dir.join("capture.pcapng").exists());
        assert!(tcp_dir.join("sdp.txt").exists());
        assert!(tcp_dir.join("protocol_transcript.json").exists());
        assert!(tcp_dir.join("capture.json").exists());
        assert!(tcp_dir.join("video.h264").exists());
        assert!(tcp_dir.join("muxed.mp4").exists());
        drop(server);
    }

    #[test]
    fn captures_onvif_and_reolink_http_exchanges() {
        let server = rouille::Server::new("127.0.0.1:0", |req| {
            let url = req.url();
            if url.contains("api.cgi") {
                let cmd = req.get_param("cmd").unwrap_or_default();
                let val = match cmd.as_str() {
                    "Login" => serde_json::json!({ "Token": { "name": "tok-reo" } }),
                    _ => serde_json::json!({ "ok": true }),
                };
                return rouille::Response::json(
                    &serde_json::json!([{ "cmd": cmd, "code": 0, "value": val }]),
                );
            }
            let body = r#"<?xml version="1.0" encoding="utf-8"?><s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:tds="http://www.onvif.org/ver10/device/wsdl"><s:Body><tds:GetDeviceInformationResponse><tds:Manufacturer>TestCam</tds:Manufacturer><tds:Model>T1</tds:Model><tds:FirmwareVersion>1.0</tds:FirmwareVersion><tds:SerialNumber>S1</tds:SerialNumber><tds:HardwareId>H1</tds:HardwareId></tds:GetDeviceInformationResponse></s:Body></s:Envelope>"#;
            rouille::Response::text(body).with_additional_header("Content-Type", "application/soap+xml")
        })
        .unwrap();
        let addr = server.server_addr();
        let (handle, stop) = server.stoppable();

        let root = TempDir::new();
        let store = CaptureStore::new(root.path(), StoreLimits::default()).unwrap();

        let mut onvif_req = CaptureRequest::new(
            "onvif-1",
            "cam-onvif",
            CaptureProtocol::Onvif,
            format!("http://{addr}/onvif/device_service"),
        );
        onvif_req.username = "test".to_owned();
        onvif_req.password = "test".to_owned();
        onvif_req.trigger_probe = false;
        let onvif_res = run_capture(&store, &onvif_req).unwrap();
        assert!(!onvif_res.transcript.http.is_empty());
        assert!(onvif_res.capture_dir.join("capture.pcapng").exists());

        let mut reo_http = CaptureRequest::new(
            "reolink-http-1",
            "cam-reo-http",
            CaptureProtocol::ReolinkHttp,
            format!("http://{addr}/cgi-bin/api.cgi"),
        );
        reo_http.username = "test".to_owned();
        reo_http.password = "test".to_owned();
        let http_res = run_capture(&store, &reo_http).unwrap();
        assert!(
            http_res
                .transcript
                .http
                .iter()
                .any(|ex| ex.path.contains("Login"))
        );

        let _ = stop.send(());
        let _ = handle.join();
    }

    #[test]
    fn captures_baichuan_tcp_session_and_bitstream() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || serve_fake_baichuan(listener));

        let root = TempDir::new();
        let store = CaptureStore::new(root.path(), StoreLimits::default()).unwrap();
        let mut req = CaptureRequest::new(
            "reo-proto-1",
            "cam-reo-bc",
            CaptureProtocol::ReoProto,
            format!("reo://{addr}"),
        );
        req.username = "test".to_owned();
        req.password = "test".to_owned();
        req.duration_max_ms = 2_000;
        req.frames_max = 1;

        let res = run_capture(&store, &req).unwrap();
        assert!(!res.transcript.reo_proto.is_empty());
        assert_eq!(res.bitstream.anomalies.video_frames, 1);
        assert!(res.capture_dir.join("video.h264").exists());
        server.join().unwrap();
    }

    fn serve_fake_baichuan(listener: TcpListener) {
        let (mut sock, _) = listener.accept().unwrap();
        let mut rx = [0_u8; 4096];
        let _ = sock.read(&mut rx);
        let mut nonce_xml = [0_u8; MAX_XML_BODY];
        let nlen =
            auth::build_nonce_response("NONCE1", EncryptionMode::None, &mut nonce_xml).unwrap();
        encryption::bc_xor(&mut nonce_xml[..nlen], 0);
        write_bc_packet(
            &mut sock,
            COMMAND_LOGIN,
            &nonce_xml[..nlen],
            make_status(BC_CLASS_LEGACY, 0xDD00),
            0,
            None,
        );
        let _ = sock.read(&mut rx);
        let mut id = CameraIdentity::default();
        id.model.push_str("RLC-Test");
        id.channel_count = 1;
        let mut ok_xml = [0_u8; MAX_XML_BODY];
        let olen = auth::build_login_confirmation(1, &id, &mut ok_xml).unwrap();
        encryption::bc_xor(&mut ok_xml[..olen], 0);
        write_bc_packet(
            &mut sock,
            COMMAND_LOGIN,
            &ok_xml[..olen],
            make_status(BC_CLASS_MODERN_EXT, 0),
            0,
            Some(0),
        );
        let _ = sock.read(&mut rx);
        let stream_ack =
            b"<body><Preview version=\"1.1\"><channelId>0</channelId><handle>0</handle><streamType>mainStream</streamType></Preview></body>";
        write_bc_packet(
            &mut sock,
            COMMAND_STREAM,
            stream_ack,
            make_status(BC_CLASS_MODERN_EXT, 0),
            0,
            Some(0),
        );
        let media = build_fake_baichuan_video_payload();
        write_bc_packet(
            &mut sock,
            COMMAND_STREAM,
            &media,
            make_status(BC_CLASS_LEGACY, 0),
            0,
            None,
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    fn write_bc_packet(
        sock: &mut TcpStream,
        msg_id: u32,
        body: &[u8],
        status_class: u32,
        encryption_offset: u32,
        extension: Option<u32>,
    ) {
        let hdr = PacketHeader {
            msg_id,
            body_len: u32::try_from(body.len()).unwrap(),
            encryption_offset,
            status_class,
            extension,
        };
        let mut buf = [0_u8; HEADER_LEN_EXTENDED];
        let hlen = hdr.serialize(&mut buf);
        sock.write_all(&buf[..hlen]).unwrap();
        sock.write_all(body).unwrap();
        sock.flush().unwrap();
    }

    fn build_fake_baichuan_video_payload() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&MEDIA_MAGIC_INFO_V1.to_le_bytes());
        data.extend_from_slice(&30_u32.to_le_bytes());
        data.extend_from_slice(&640_u32.to_le_bytes());
        data.extend_from_slice(&360_u32.to_le_bytes());
        data.push(0);
        data.push(15);
        data.extend_from_slice(&[26, 1, 1, 0, 0, 0, 26, 1, 1, 0, 0, 0]);
        data.resize((data.len() + 7) & !7, 0);
        let nalu = [0_u8, 0, 0, 1, 0x65, 0x88, 0x84, 0x21];
        data.extend_from_slice(&MEDIA_MAGIC_IFRAME_BASE.to_le_bytes());
        data.extend_from_slice(b"H264");
        data.extend_from_slice(&(nalu.len() as u32).to_le_bytes());
        data.extend_from_slice(&0_u32.to_le_bytes());
        data.extend_from_slice(&1_000_u32.to_le_bytes());
        data.extend_from_slice(&0_u32.to_le_bytes());
        data.extend_from_slice(&nalu);
        data.resize((data.len() + 7) & !7, 0);
        data
    }

    #[test]
    fn captures_isapi_digest_and_redacts_when_include_raw_auth_is_false() {
        let device = FakeHikvision::builder()
            .credentials("admin", "secret-pass")
            .replies([
                HikvisionReply::http(
                    200,
                    "application/xml",
                    "<DeviceInfo><model>DS-Test</model></DeviceInfo>",
                ),
                HikvisionReply::http(
                    200,
                    "application/xml",
                    "<DeviceStatus><cpu>12</cpu></DeviceStatus>",
                ),
            ])
            .start()
            .unwrap();
        let root = TempDir::new();
        let store = CaptureStore::new(root.path(), StoreLimits::default()).unwrap();
        let mut req = CaptureRequest::new(
            "isapi-1",
            "cam-isapi",
            CaptureProtocol::Isapi,
            device.origin(),
        );
        req.username = "admin".to_owned();
        req.password = "secret-pass".to_owned();
        req.include_raw_auth = false;

        let res = run_capture(&store, &req).unwrap();
        assert!(!res.transcript.http.is_empty());
        let transcript_text =
            std::fs::read_to_string(res.capture_dir.join("protocol_transcript.json")).unwrap();
        assert!(!transcript_text.contains("Digest username="));
        let pcap_bytes = std::fs::read(res.capture_dir.join("capture.pcapng")).unwrap();
        assert!(!String::from_utf8_lossy(&pcap_bytes).contains("Digest username="));
    }

    fn serve_one_https_json_request(
        listener: &TcpListener,
        config: &Arc<rustls::ServerConfig>,
        body: &str,
    ) {
        let Ok((tcp, _)) = listener.accept() else {
            return;
        };
        let _ = tcp.set_read_timeout(Some(Duration::from_secs(2)));
        let conn = rustls::ServerConnection::new(Arc::clone(config)).unwrap();
        let mut tls = rustls::StreamOwned::new(conn, tcp);
        let mut req_buf = Vec::new();
        let mut chunk = [0_u8; 2048];
        let mut expected_total: Option<usize> = None;
        while expected_total.is_none_or(|need| req_buf.len() < need) {
            match tls.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    req_buf.extend_from_slice(&chunk[..n]);
                    if expected_total.is_none()
                        && let Some(hdr_end) = req_buf.windows(4).position(|w| w == b"\r\n\r\n")
                    {
                        let hdr_str = String::from_utf8_lossy(&req_buf[..hdr_end]);
                        let content_len = hdr_str
                            .lines()
                            .find_map(|line| {
                                let (k, v) = line.split_once(':')?;
                                k.trim()
                                    .eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        expected_total = Some(hdr_end + 4 + content_len);
                    }
                }
            }
        }
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = tls.write_all(resp.as_bytes());
        let _ = tls.flush();
        tls.conn.send_close_notify();
        let _ = tls.conn.complete_io(&mut tls.sock);
        let _ = tls.sock.shutdown(std::net::Shutdown::Write);
    }

    #[test]
    fn captures_decrypted_https_traffic_to_pcapng() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let certified =
            rcgen::generate_simple_self_signed(["localhost".to_owned(), "127.0.0.1".to_owned()])
                .unwrap();
        let cert_der = certified.cert.der().clone();
        let key_der =
            rustls::pki_types::PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der());
        let config = Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(vec![cert_der], key_der.into())
                .unwrap(),
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let body = r#"[{"cmd":"Login","code":0,"value":{"Token":{"name":"tok123"}}}]"#;
            for _ in 0..3 {
                serve_one_https_json_request(&listener, &config, body);
            }
        });

        let root = TempDir::new();
        let store = CaptureStore::new(root.path(), StoreLimits::default()).unwrap();
        let mut req = CaptureRequest::new(
            "https-tap-1",
            "cam-https",
            CaptureProtocol::ReolinkHttp,
            format!("https://{addr}/cgi-bin/api.cgi"),
        );
        req.username = "admin".to_owned();
        req.password = "pw".to_owned();
        req.trigger_probe = false;
        req.tls_accept_invalid_certs = true;

        let res = run_capture(&store, &req).unwrap();
        assert!(res.metadata.tls_decrypted);
        assert!(res.transcript.tls_decrypted);
        let pcap_bytes = std::fs::read(res.capture_dir.join("capture.pcapng")).unwrap();
        let pcap_lossy = String::from_utf8_lossy(&pcap_bytes);
        assert!(pcap_lossy.contains("tls_decrypted=true"));
        assert!(pcap_lossy.contains("tok123"));
        server.join().unwrap();
    }

    #[test]
    fn redacts_http_onvif_and_baichuan_when_include_raw_auth_false() {
        let root = TempDir::new();
        let store = CaptureStore::new(root.path(), StoreLimits::default()).unwrap();
        assert_http_and_onvif_redacted(&store);
        assert_baichuan_login_redacted(&store);
    }

    fn assert_http_and_onvif_redacted(store: &CaptureStore) {
        let server = rouille::Server::new("127.0.0.1:0", |req| {
            if req.url().contains("api.cgi") {
                return rouille::Response::json(
                    &serde_json::json!([{ "cmd": "Login", "code": 0, "value": { "Token": { "name": "tok-secret-99" } } }]),
                );
            }
            rouille::Response::text(
                "<s:Envelope><s:Body><GetDeviceInformationResponse/></s:Body></s:Envelope>",
            )
        })
        .unwrap();
        let http_addr = server.server_addr();
        let (handle, stop) = server.stoppable();
        for (id, proto, url, pw) in [
            (
                "reo-red",
                CaptureProtocol::ReolinkHttp,
                format!("http://{http_addr}/cgi-bin/api.cgi"),
                "reo-pass-99",
            ),
            (
                "onv-red",
                CaptureProtocol::Onvif,
                format!("http://{http_addr}/onvif/device_service"),
                "onvif-pass-99",
            ),
        ] {
            let mut req = CaptureRequest::new(id, "cam-red", proto, url);
            req.username = "camadmin".to_owned();
            req.password = pw.to_owned();
            req.include_raw_auth = false;
            req.trigger_probe = false;
            let res = run_capture(store, &req).unwrap();
            let pcap = String::from_utf8_lossy(
                &std::fs::read(res.capture_dir.join("capture.pcapng")).unwrap(),
            )
            .into_owned();
            let json =
                std::fs::read_to_string(res.capture_dir.join("protocol_transcript.json")).unwrap();
            for leak in [pw, "tok-secret-99", "camadmin"] {
                assert!(!pcap.contains(leak), "pcap leaked {leak}");
                assert!(!json.contains(leak), "transcript leaked {leak}");
            }
        }
        let _ = stop.send(());
        let _ = std::net::TcpStream::connect(http_addr);
        let _ = handle.join();
    }

    fn assert_baichuan_login_redacted(store: &CaptureStore) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let bc_addr = listener.local_addr().unwrap();
        let bc_srv = std::thread::spawn(move || serve_fake_baichuan(listener));
        let mut bc_req = CaptureRequest::new(
            "bc-red",
            "cam-bc",
            CaptureProtocol::ReoProto,
            format!("reo://{bc_addr}"),
        );
        bc_req.username = "bcadmin".to_owned();
        bc_req.password = "bc-pass-99".to_owned();
        bc_req.include_raw_auth = false;
        bc_req.duration_max_ms = 2_000;
        bc_req.frames_max = 1;
        let bc_res = run_capture(store, &bc_req).unwrap();
        assert!(
            bc_res
                .transcript
                .reo_proto
                .iter()
                .filter(|f| f.msg_id == 1)
                .all(|f| f.payload_base64.is_none())
        );
        let bc_pcap = String::from_utf8_lossy(
            &std::fs::read(bc_res.capture_dir.join("capture.pcapng")).unwrap(),
        )
        .into_owned();
        assert!(!bc_pcap.contains("bcadmin"));
        assert!(!bc_pcap.contains("bc-pass-99"));
        bc_srv.join().unwrap();
    }

    #[test]
    fn handles_split_rtsp_http_overflow_and_tight_capture_budget() {
        let mut overflow =
            b"RTSP/1.0 200 OK\r\nContent-Length: 18446744073709551615\r\n\r\nbody".to_vec();
        assert!(try_take_rtsp_or_http_message(&mut overflow).is_none());
        assert!(decode_chunked_body(b"ffffffffffffffff\r\n").is_none());

        let mut mixed =
            b"RTSP/1.0 200 OK\r\nContent-Length: 4\r\n\r\nping$\x00\x00\x04ABCD".to_vec();
        let msg = try_take_rtsp_or_http_message(&mut mixed).unwrap();
        assert_eq!(msg.body, b"ping");
        assert_eq!(mixed, b"$\x00\x00\x04ABCD");

        let server = RtspServer::from_mp4(fixture_mp4("cc-4k-640x360-h264.mp4")).unwrap();
        let root = TempDir::new();
        let limits = StoreLimits {
            max_capture_bytes: 65_536,
            max_storage_bytes: 512 * 1024,
            ..StoreLimits::default()
        };
        let store = CaptureStore::new(root.path(), limits).unwrap();
        let mut req = CaptureRequest::new(
            "rtsp-tight",
            "cam-tight",
            CaptureProtocol::Rtsp,
            server.url().as_str(),
        );
        req.username = "op".to_owned();
        req.password = "pw-tight".to_owned();
        req.include_raw_auth = false;
        req.duration_max_ms = 1_500;
        req.frames_max = 60;
        let res = run_capture(&store, &req).unwrap();
        assert!(res.metadata.total_bytes <= 65_536);
        let pcap = String::from_utf8_lossy(
            &std::fs::read(res.capture_dir.join("capture.pcapng")).unwrap(),
        )
        .into_owned();
        assert!(!pcap.contains("pw-tight"));
        drop(server);
    }
}
