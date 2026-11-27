//! Offline replay server for `keeppeek.camera-repro.v1` camera bug fixtures.

use anyhow::{Context, bail, ensure};
use reo_proto::{
    COMMAND_LOGIN, COMMAND_PING, COMMAND_STREAM, PacketHeader, ReadBuffer,
    auth::EncryptionMode,
    encryption,
    magic::{BC_CLASS_LEGACY, BC_CLASS_MODERN_EXT, HEADER_LEN_EXTENDED, make_status},
};
use retina::{client::core::RtspFramer, rtsp::msg};
use serde_json::{Map, Value};
use std::{
    fs::File,
    io::{self, Read, Write},
    net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// Schema identifier required on every camera reproduction fixture.
pub const REPRO_SCHEMA_VERSION: &str = "keeppeek.camera-repro.v1";

/// Maximum fixture file size accepted by [`CameraReproFixture::from_file`].
pub const REPRO_FIXTURE_BYTES_MAX: usize = 32 * 1024 * 1024;

const SESSIONS_MAX: usize = 64;
const RESPONSES_PER_SESSION_MAX: usize = 256;
const RTP_PACKETS_PER_SESSION_MAX: usize = 100_000;
const RTP_PAYLOAD_BYTES_MAX: usize = 65_523;
const HTTP_EXCHANGES_MAX: usize = 1024;
const REO_FRAMES_MAX: usize = 10_000;
const HEADERS_PER_MESSAGE_MAX: usize = 128;
const DELAY_MS_MAX: u64 = 60_000;
const RECORDED_REQUESTS_MAX: usize = 4096;
const CONCURRENT_CONNECTIONS_MAX: usize = 32;
const HTTP_HEADER_BYTES_MAX: usize = 64 * 1024;
const HTTP_BODY_BYTES_MAX: usize = 4 * 1024 * 1024;
const RTSP_MESSAGE_BYTES_MAX: usize = 256 * 1024;
const REO_BODY_BYTES_MAX: usize = 16 * 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const IO_TIMEOUT: Duration = Duration::from_millis(100);
const DEFAULT_SESSION_ID: &str = "replay-session";

/// Protocol class represented by a [`CameraReproFixture`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReproProtocol {
    /// RTSP control and RTP packet delivery.
    Rtsp,
    /// ONVIF SOAP over HTTP.
    Onvif,
    /// Hikvision ISAPI over HTTP.
    Isapi,
    /// Reolink HTTP JSON API.
    ReolinkHttp,
    /// Reolink Baichuan binary protocol.
    ReoProto,
}

impl ReproProtocol {
    /// Parses a protocol token from a fixture JSON string.
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "rtsp" => Ok(Self::Rtsp),
            "onvif" => Ok(Self::Onvif),
            "isapi" => Ok(Self::Isapi),
            "reolink_http" => Ok(Self::ReolinkHttp),
            "reo_proto" => Ok(Self::ReoProto),
            other => bail!("unsupported camera protocol: {other}"),
        }
    }

    /// Returns the canonical schema string for this protocol.
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

/// Camera metadata section inside [`CameraReproFixture`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReproCameraInfo {
    /// Optional human-readable camera label.
    pub name: Option<String>,
    /// Primary camera protocol captured in this fixture.
    pub protocol: ReproProtocol,
    /// Whether network addresses were anonymized before export.
    pub anonymized: bool,
    /// Whether credentials were stripped before export.
    pub credentials_stripped: bool,
    /// Host or IP recorded in the exported fixture.
    pub base_host: String,
}

/// Scripted RTSP response rule for one control request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtspResponseFixture {
    /// Optional case-insensitive RTSP method matcher.
    pub method: Option<String>,
    /// Optional substring matcher against the request URI.
    pub uri_contains: Option<String>,
    /// Numeric RTSP status code.
    pub status: u16,
    /// RTSP status reason phrase.
    pub reason: String,
    /// Ordered RTSP headers emitted in the response.
    pub headers: Vec<(String, String)>,
    /// Optional response body text such as an SDP description.
    pub body: String,
}

/// Exact RTP packet entry emitted during an RTSP `PLAY` session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtpPacketFixture {
    /// Optional default TCP interleaved channel number.
    pub channel: Option<u8>,
    /// Logical track identifier matched to `SETUP` order or URI.
    pub track: String,
    /// RTP marker bit.
    pub marker: bool,
    /// Seven-bit RTP payload type.
    pub payload_type: u8,
    /// Wire RTP sequence number.
    pub sequence: u16,
    /// Wire RTP timestamp.
    pub timestamp: u32,
    /// Wire RTP synchronization source identifier.
    pub ssrc: u32,
    /// Optional delay in milliseconds before sending this packet.
    pub delay_ms: Option<u64>,
    /// Base64-encoded RTP payload bytes after the 12-byte header.
    pub payload_base64: String,
}

/// One RTSP session script inside [`RtspReproFixture`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtspSessionFixture {
    /// Optional ordered track names matched sequentially during `SETUP`.
    pub setup_order: Option<Vec<String>>,
    /// Ordered RTSP control responses.
    pub responses: Vec<RtspResponseFixture>,
    /// Ordered RTP packets emitted after a successful `PLAY`.
    pub rtp_packets: Vec<RtpPacketFixture>,
}

/// RTSP replay section inside [`CameraReproFixture`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtspReproFixture {
    /// Recorded RTSP sessions served by the replay server.
    pub sessions: Vec<RtspSessionFixture>,
}

/// One deterministic HTTP request-response exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpExchangeFixture {
    /// Optional case-insensitive HTTP method matcher.
    pub match_method: Option<String>,
    /// Optional substring matcher against request path and query.
    pub match_path_contains: Option<String>,
    /// Optional matcher against `SOAPAction` or `Content-Type` action parameter.
    pub match_soap_action: Option<String>,
    /// Optional substring matcher against UTF-8-lossy request body.
    pub match_body_contains: Option<String>,
    /// HTTP status code returned by the replay server.
    pub status: u16,
    /// Optional custom HTTP reason phrase.
    pub status_reason: Option<String>,
    /// Ordered HTTP headers returned in the response.
    pub headers: Vec<(String, String)>,
    /// Optional UTF-8 response body text.
    pub body_text: Option<String>,
    /// Optional base64-encoded binary response body.
    pub body_base64: Option<String>,
    /// Optional delay in milliseconds before sending the response.
    pub delay_ms: Option<u64>,
}

/// HTTP replay section inside [`CameraReproFixture`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpReproFixture {
    /// Ordered HTTP exchanges matched against incoming requests.
    pub exchanges: Vec<HttpExchangeFixture>,
}

/// One Reolink Baichuan TCP frame exchange or media frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReoFrameFixture {
    /// Optional incoming Baichuan `msg_id` awaited before emitting this frame.
    pub expected_msg_id: Option<u32>,
    /// Baichuan `msg_id` written in the outgoing packet header.
    pub response_msg_id: u32,
    /// Optional raw base64-encoded body bytes.
    pub payload_base64: Option<String>,
    /// Optional UTF-8 XML body encoded according to Baichuan rules.
    pub payload_xml: Option<String>,
    /// Optional Baichuan channel identifier encoded in the header.
    pub channel_id: Option<u8>,
    /// Optional Baichuan stream type identifier encoded in the header.
    pub stream_type: Option<u8>,
    /// Optional delay in milliseconds before sending this frame.
    pub delay_ms: Option<u64>,
}

/// Reolink Baichuan replay section inside [`CameraReproFixture`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReoProtoReproFixture {
    /// Ordered Baichuan frames emitted on TCP connections.
    pub frames: Vec<ReoFrameFixture>,
}

/// Parsed and bounded `keeppeek.camera-repro.v1` fixture document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraReproFixture {
    /// Schema version string, which must equal [`REPRO_SCHEMA_VERSION`].
    pub schema_version: String,
    /// Camera identification and sanitization metadata.
    pub camera: ReproCameraInfo,
    /// Optional RTSP/RTP replay script.
    pub rtsp: Option<RtspReproFixture>,
    /// Optional HTTP/ONVIF/ISAPI/Reolink-HTTP replay script.
    pub http: Option<HttpReproFixture>,
    /// Optional Reolink Baichuan replay script.
    pub reo_proto: Option<ReoProtoReproFixture>,
}

impl CameraReproFixture {
    /// Loads and validates a fixture JSON file with a strict 32 MiB cap.
    ///
    /// # Errors
    /// Returns an error when the file cannot be read, exceeds 32 MiB, or fails validation.
    pub fn from_file(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let meta = std::fs::metadata(path)
            .with_context(|| format!("unable to stat fixture {}", path.display()))?;
        ensure!(
            meta.len() <= REPRO_FIXTURE_BYTES_MAX as u64,
            "fixture {} exceeds {} bytes",
            path.display(),
            REPRO_FIXTURE_BYTES_MAX
        );
        let file = File::open(path)
            .with_context(|| format!("unable to open fixture {}", path.display()))?;
        let mut bytes = Vec::new();
        let limit = (REPRO_FIXTURE_BYTES_MAX as u64).saturating_add(1);
        file.take(limit)
            .read_to_end(&mut bytes)
            .with_context(|| format!("unable to read fixture {}", path.display()))?;
        Self::from_json_bytes(&bytes)
    }

    /// Parses and validates a fixture from JSON bytes.
    ///
    /// # Errors
    /// Returns an error when the slice exceeds 32 MiB, is invalid JSON, or fails validation.
    pub fn from_json_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        ensure!(
            bytes.len() <= REPRO_FIXTURE_BYTES_MAX,
            "fixture exceeds {REPRO_FIXTURE_BYTES_MAX} bytes"
        );
        let value: Value =
            serde_json::from_slice(bytes).context("unable to parse repro fixture JSON")?;
        Self::from_json_value(&value)
    }

    /// Parses and validates a fixture from a JSON string.
    ///
    /// # Errors
    /// Returns an error when the string exceeds 32 MiB, is invalid JSON, or fails validation.
    pub fn from_json_str(json: &str) -> anyhow::Result<Self> {
        Self::from_json_bytes(json.as_bytes())
    }

    /// Parses and validates a fixture from a [`serde_json::Value`].
    ///
    /// # Errors
    /// Returns an error when required fields are missing or fail schema validation.
    pub fn from_json_value(value: &Value) -> anyhow::Result<Self> {
        let obj = value
            .as_object()
            .context("fixture root must be a JSON object")?;
        let schema_version = req_str(obj, "schema_version")?;
        let camera_val = obj
            .get("camera")
            .and_then(Value::as_object)
            .context("fixture is missing object field 'camera'")?;
        let camera = parse_camera_info(camera_val)?;
        let rtsp = obj
            .get("rtsp")
            .filter(|v| !v.is_null())
            .map(parse_rtsp)
            .transpose()?;
        let http = obj
            .get("http")
            .filter(|v| !v.is_null())
            .map(parse_http)
            .transpose()?;
        let reo_proto = obj
            .get("reo_proto")
            .filter(|v| !v.is_null())
            .map(parse_reo)
            .transpose()?;
        let fixture = Self {
            schema_version,
            camera,
            rtsp,
            http,
            reo_proto,
        };
        fixture.validate()?;
        Ok(fixture)
    }

    /// Serializes this fixture to a [`serde_json::Value`].
    pub fn to_json_value(&self) -> Value {
        let mut root = Map::new();
        root.insert(
            "schema_version".to_owned(),
            Value::String(self.schema_version.clone()),
        );
        root.insert("camera".to_owned(), camera_to_json(&self.camera));
        if let Some(rtsp) = &self.rtsp {
            root.insert("rtsp".to_owned(), rtsp_to_json(rtsp));
        }
        if let Some(http) = &self.http {
            root.insert("http".to_owned(), http_to_json(http));
        }
        if let Some(reo) = &self.reo_proto {
            root.insert("reo_proto".to_owned(), reo_to_json(reo));
        }
        Value::Object(root)
    }

    /// Serializes this fixture to compact JSON text.
    ///
    /// # Errors
    /// Returns an error when validation or JSON serialization fails.
    pub fn to_json_string(&self) -> anyhow::Result<String> {
        self.validate()?;
        serde_json::to_string(&self.to_json_value()).context("unable to serialize repro fixture")
    }

    /// Validates schema version, bounds, and payload encodings.
    ///
    /// # Errors
    /// Returns an error when any invariant or safety bound is violated.
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.schema_version == REPRO_SCHEMA_VERSION,
            "unsupported fixture schema_version '{}'; expected '{REPRO_SCHEMA_VERSION}'",
            self.schema_version
        );
        ensure!(
            !self.camera.base_host.trim().is_empty() && self.camera.base_host.len() <= 256,
            "camera.base_host must be between 1 and 256 bytes"
        );
        if let Some(rtsp) = &self.rtsp {
            validate_rtsp(rtsp)?;
        }
        if let Some(http) = &self.http {
            validate_http(http)?;
        }
        if let Some(reo) = &self.reo_proto {
            validate_reo(reo)?;
        }
        Ok(())
    }
}

/// Protocol transport captured for a [`RecordedRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordedProtocol {
    /// Request received on the HTTP replay listener.
    Http,
    /// Request received on the RTSP replay listener.
    Rtsp,
    /// Frame received on the Reolink Baichuan TCP replay listener.
    ReoProto,
}

/// Incoming client request captured by [`ReplayCameraServer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRequest {
    /// Protocol listener that received the request.
    pub protocol: RecordedProtocol,
    /// Method name or Baichuan message identifier.
    pub method: String,
    /// Request path, RTSP URI, or Baichuan target metadata.
    pub target: String,
    /// Ordered request headers or decoded header fields.
    pub headers: Vec<(String, String)>,
    /// Raw request body bytes.
    pub body: Vec<u8>,
}

/// Startup options for [`ReplayCameraServer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayServerOptions {
    /// Preferred bind socket address; port `0` assigns ephemeral ports per active protocol.
    pub bind_addr: SocketAddr,
    /// Repeats scripted exchanges, RTP packets, and Baichuan frames across requests.
    pub loop_replay: bool,
}

impl Default for ReplayServerOptions {
    fn default() -> Self {
        Self {
            bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            loop_replay: false,
        }
    }
}

/// Deterministic loopback replay server for [`CameraReproFixture`].
pub struct ReplayCameraServer {
    http: Option<EndpointHandle>,
    rtsp: Option<EndpointHandle>,
    reo: Option<EndpointHandle>,
    shared: Arc<ReplayShared>,
}

struct EndpointHandle {
    addr: SocketAddr,
    worker: Option<JoinHandle<()>>,
}

struct ReplayShared {
    stop: AtomicBool,
    loop_replay: bool,
    recorded: Mutex<Vec<RecordedRequest>>,
    http_done: AtomicBool,
    rtsp_done: AtomicBool,
    reo_done: AtomicBool,
}

impl ReplayShared {
    fn record(&self, request: RecordedRequest) {
        let mut recorded = self
            .recorded
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if recorded.len() < RECORDED_REQUESTS_MAX {
            recorded.push(request);
        }
    }
}

impl ReplayCameraServer {
    /// Loads a fixture file and starts replay listeners on `127.0.0.1:0`.
    ///
    /// # Errors
    /// Returns an error when loading fails or a listener cannot bind.
    pub fn from_file(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        Self::from_file_with_options(path, ReplayServerOptions::default())
    }

    /// Loads a fixture file and starts replay listeners with explicit options.
    ///
    /// # Errors
    /// Returns an error when loading fails or a listener cannot bind.
    pub fn from_file_with_options(
        path: impl AsRef<Path>,
        options: ReplayServerOptions,
    ) -> anyhow::Result<Self> {
        let fixture = CameraReproFixture::from_file(path)?;
        Self::start_with_options(fixture, options)
    }

    /// Validates `fixture` and starts active protocol listeners on `127.0.0.1:0`.
    ///
    /// # Errors
    /// Returns an error when validation fails or a listener cannot bind.
    pub fn start(fixture: CameraReproFixture) -> anyhow::Result<Self> {
        Self::start_with_options(fixture, ReplayServerOptions::default())
    }

    /// Validates `fixture` and starts active protocol listeners with `options`.
    ///
    /// # Errors
    /// Returns an error when validation fails or a listener cannot bind.
    pub fn start_with_options(
        fixture: CameraReproFixture,
        options: ReplayServerOptions,
    ) -> anyhow::Result<Self> {
        fixture.validate()?;
        let shared = Arc::new(ReplayShared {
            stop: AtomicBool::new(false),
            loop_replay: options.loop_replay,
            recorded: Mutex::new(Vec::new()),
            http_done: AtomicBool::new(
                fixture
                    .http
                    .as_ref()
                    .is_none_or(|http| http.exchanges.is_empty()),
            ),
            rtsp_done: AtomicBool::new(
                fixture
                    .rtsp
                    .as_ref()
                    .is_none_or(|rtsp| rtsp.sessions.is_empty()),
            ),
            reo_done: AtomicBool::new(
                fixture
                    .reo_proto
                    .as_ref()
                    .is_none_or(|reo| reo.frames.is_empty()),
            ),
        });
        let (http_addr, rtsp_addr, reo_addr) =
            plan_bind_addresses(options.bind_addr, fixture.camera.protocol);
        let http = fixture
            .http
            .map(|http| start_http_listener(http_addr, http, Arc::clone(&shared)))
            .transpose()?;
        let rtsp = fixture
            .rtsp
            .map(|rtsp| start_rtsp_listener(rtsp_addr, rtsp, Arc::clone(&shared)))
            .transpose()?;
        let reo = fixture
            .reo_proto
            .map(|reo| start_reo_listener(reo_addr, reo, Arc::clone(&shared)))
            .transpose()?;
        Ok(Self {
            http,
            rtsp,
            reo,
            shared,
        })
    }

    /// Returns the bound HTTP socket address if an HTTP section is active.
    pub const fn http_addr(&self) -> Option<SocketAddr> {
        match &self.http {
            Some(handle) => Some(handle.addr),
            None => None,
        }
    }

    /// Returns the bound RTSP socket address if an RTSP section is active.
    pub const fn rtsp_addr(&self) -> Option<SocketAddr> {
        match &self.rtsp {
            Some(handle) => Some(handle.addr),
            None => None,
        }
    }

    /// Returns the bound Reolink Baichuan socket address if active.
    pub const fn reo_addr(&self) -> Option<SocketAddr> {
        match &self.reo {
            Some(handle) => Some(handle.addr),
            None => None,
        }
    }

    /// Returns the `http://` URL if an HTTP section is active.
    pub fn http_url(&self) -> Option<String> {
        self.http_addr().map(|addr| format!("http://{addr}"))
    }

    /// Returns the `rtsp://` URL if an RTSP section is active.
    pub fn rtsp_url(&self) -> Option<String> {
        self.rtsp_addr().map(|addr| format!("rtsp://{addr}"))
    }

    /// Returns the `reo://` address URL if a Baichuan section is active.
    pub fn reo_url(&self) -> Option<String> {
        self.reo_addr().map(|addr| format!("reo://{addr}"))
    }

    /// Returns a snapshot of captured client requests across all active listeners.
    pub fn recorded_requests(&self) -> Vec<RecordedRequest> {
        self.shared
            .recorded
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Returns whether all active protocol scripts finished one complete pass.
    pub fn is_completed(&self) -> bool {
        self.shared.http_done.load(Ordering::Acquire)
            && self.shared.rtsp_done.load(Ordering::Acquire)
            && self.shared.reo_done.load(Ordering::Acquire)
    }
}

impl Drop for ReplayCameraServer {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        for endpoint in [&mut self.http, &mut self.rtsp, &mut self.reo] {
            if let Some(mut handle) = endpoint.take()
                && let Some(worker) = handle.worker.take()
            {
                let _ = worker.join();
            }
        }
    }
}

const fn plan_bind_addresses(
    bind: SocketAddr,
    primary: ReproProtocol,
) -> (SocketAddr, SocketAddr, SocketAddr) {
    let ephemeral = SocketAddr::new(bind.ip(), 0);
    if bind.port() == 0 {
        return (ephemeral, ephemeral, ephemeral);
    }
    match primary {
        ReproProtocol::Rtsp => (ephemeral, bind, ephemeral),
        ReproProtocol::Onvif | ReproProtocol::Isapi | ReproProtocol::ReolinkHttp => {
            (bind, ephemeral, ephemeral)
        }
        ReproProtocol::ReoProto => (ephemeral, ephemeral, bind),
    }
}

fn parse_camera_info(obj: &Map<String, Value>) -> anyhow::Result<ReproCameraInfo> {
    Ok(ReproCameraInfo {
        name: opt_str(obj, "name")?,
        protocol: ReproProtocol::parse(&req_str(obj, "protocol")?)?,
        anonymized: req_bool(obj, "anonymized")?,
        credentials_stripped: req_bool(obj, "credentials_stripped")?,
        base_host: req_str(obj, "base_host")?,
    })
}

fn parse_rtsp(value: &Value) -> anyhow::Result<RtspReproFixture> {
    let obj = value.as_object().context("'rtsp' must be a JSON object")?;
    let arr = obj
        .get("sessions")
        .and_then(Value::as_array)
        .context("'rtsp.sessions' must be an array")?;
    ensure!(arr.len() <= SESSIONS_MAX, "too many RTSP sessions");
    let mut sessions = Vec::with_capacity(arr.len());
    for item in arr {
        sessions.push(parse_rtsp_session(item)?);
    }
    Ok(RtspReproFixture { sessions })
}

fn parse_rtsp_session(value: &Value) -> anyhow::Result<RtspSessionFixture> {
    let obj = value
        .as_object()
        .context("RTSP session must be a JSON object")?;
    let setup_order = obj
        .get("setup_order")
        .filter(|v| !v.is_null())
        .map(parse_string_list)
        .transpose()?;
    let resp_arr = obj
        .get("responses")
        .and_then(Value::as_array)
        .context("RTSP session is missing 'responses' array")?;
    ensure!(
        resp_arr.len() <= RESPONSES_PER_SESSION_MAX,
        "too many RTSP responses"
    );
    let mut responses = Vec::with_capacity(resp_arr.len());
    for item in resp_arr {
        responses.push(parse_rtsp_response(item)?);
    }
    let pkt_arr = obj
        .get("rtp_packets")
        .and_then(Value::as_array)
        .context("RTSP session is missing 'rtp_packets' array")?;
    ensure!(
        pkt_arr.len() <= RTP_PACKETS_PER_SESSION_MAX,
        "too many RTP packets"
    );
    let mut rtp_packets = Vec::with_capacity(pkt_arr.len());
    for item in pkt_arr {
        rtp_packets.push(parse_rtp_packet(item)?);
    }
    Ok(RtspSessionFixture {
        setup_order,
        responses,
        rtp_packets,
    })
}

fn parse_rtsp_response(value: &Value) -> anyhow::Result<RtspResponseFixture> {
    let obj = value
        .as_object()
        .context("RTSP response must be a JSON object")?;
    Ok(RtspResponseFixture {
        method: opt_str(obj, "method")?,
        uri_contains: opt_str(obj, "uri_contains")?,
        status: req_u16(obj, "status")?,
        reason: req_str(obj, "reason")?,
        headers: parse_headers(obj.get("headers"))?,
        body: req_str(obj, "body")?,
    })
}

fn parse_rtp_packet(value: &Value) -> anyhow::Result<RtpPacketFixture> {
    let obj = value
        .as_object()
        .context("RTP packet must be a JSON object")?;
    Ok(RtpPacketFixture {
        channel: opt_u8(obj, "channel")?,
        track: req_str(obj, "track")?,
        marker: req_bool(obj, "marker")?,
        payload_type: req_u8(obj, "payload_type")?,
        sequence: req_u16(obj, "sequence")?,
        timestamp: req_u32(obj, "timestamp")?,
        ssrc: req_u32(obj, "ssrc")?,
        delay_ms: opt_u64(obj, "delay_ms")?,
        payload_base64: req_str(obj, "payload_base64")?,
    })
}

fn parse_http(value: &Value) -> anyhow::Result<HttpReproFixture> {
    let obj = value.as_object().context("'http' must be a JSON object")?;
    let arr = obj
        .get("exchanges")
        .and_then(Value::as_array)
        .context("'http.exchanges' must be an array")?;
    ensure!(arr.len() <= HTTP_EXCHANGES_MAX, "too many HTTP exchanges");
    let mut exchanges = Vec::with_capacity(arr.len());
    for item in arr {
        exchanges.push(parse_http_exchange(item)?);
    }
    Ok(HttpReproFixture { exchanges })
}

fn parse_http_exchange(value: &Value) -> anyhow::Result<HttpExchangeFixture> {
    let obj = value
        .as_object()
        .context("HTTP exchange must be a JSON object")?;
    Ok(HttpExchangeFixture {
        match_method: opt_str(obj, "match_method")?,
        match_path_contains: opt_str(obj, "match_path_contains")?,
        match_soap_action: opt_str(obj, "match_soap_action")?,
        match_body_contains: opt_str(obj, "match_body_contains")?,
        status: req_u16(obj, "status")?,
        status_reason: opt_str(obj, "status_reason")?,
        headers: parse_headers(obj.get("headers"))?,
        body_text: opt_str(obj, "body_text")?,
        body_base64: opt_str(obj, "body_base64")?,
        delay_ms: opt_u64(obj, "delay_ms")?,
    })
}

fn parse_reo(value: &Value) -> anyhow::Result<ReoProtoReproFixture> {
    let obj = value
        .as_object()
        .context("'reo_proto' must be a JSON object")?;
    let arr = obj
        .get("frames")
        .and_then(Value::as_array)
        .context("'reo_proto.frames' must be an array")?;
    ensure!(arr.len() <= REO_FRAMES_MAX, "too many reo_proto frames");
    let mut frames = Vec::with_capacity(arr.len());
    for item in arr {
        frames.push(parse_reo_frame(item)?);
    }
    Ok(ReoProtoReproFixture { frames })
}

fn parse_reo_frame(value: &Value) -> anyhow::Result<ReoFrameFixture> {
    let obj = value
        .as_object()
        .context("reo_proto frame must be a JSON object")?;
    Ok(ReoFrameFixture {
        expected_msg_id: opt_u32(obj, "expected_msg_id")?,
        response_msg_id: req_u32(obj, "response_msg_id")?,
        payload_base64: opt_str(obj, "payload_base64")?,
        payload_xml: opt_str(obj, "payload_xml")?,
        channel_id: opt_u8(obj, "channel_id")?,
        stream_type: opt_u8(obj, "stream_type")?,
        delay_ms: opt_u64(obj, "delay_ms")?,
    })
}

fn parse_headers(value: Option<&Value>) -> anyhow::Result<Vec<(String, String)>> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(Vec::new());
    };
    let arr = value.as_array().context("'headers' must be an array")?;
    ensure!(arr.len() <= HEADERS_PER_MESSAGE_MAX, "too many headers");
    let mut headers = Vec::with_capacity(arr.len());
    for pair in arr {
        let pair = pair.as_array().context("header entry must be a 2-tuple")?;
        ensure!(pair.len() == 2, "header entry must have two strings");
        let name = pair[0].as_str().context("header name must be a string")?;
        let val = pair[1].as_str().context("header value must be a string")?;
        headers.push((name.to_owned(), val.to_owned()));
    }
    Ok(headers)
}

fn parse_string_list(value: &Value) -> anyhow::Result<Vec<String>> {
    let arr = value.as_array().context("expected string array")?;
    ensure!(arr.len() <= HEADERS_PER_MESSAGE_MAX, "string list too long");
    arr.iter()
        .map(|v| {
            v.as_str()
                .map(ToOwned::to_owned)
                .context("expected string element")
        })
        .collect()
}

fn req_str(obj: &Map<String, Value>, key: &str) -> anyhow::Result<String> {
    obj.get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .with_context(|| format!("missing or non-string field '{key}'"))
}

fn opt_str(obj: &Map<String, Value>, key: &str) -> anyhow::Result<Option<String>> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => bail!("field '{key}' must be a string"),
    }
}

fn req_bool(obj: &Map<String, Value>, key: &str) -> anyhow::Result<bool> {
    obj.get(key)
        .and_then(Value::as_bool)
        .with_context(|| format!("missing or non-boolean field '{key}'"))
}

fn req_u8(obj: &Map<String, Value>, key: &str) -> anyhow::Result<u8> {
    let raw = obj
        .get(key)
        .and_then(Value::as_u64)
        .with_context(|| format!("missing or non-integer field '{key}'"))?;
    u8::try_from(raw).with_context(|| format!("field '{key}' out of u8 range"))
}

fn opt_u8(obj: &Map<String, Value>, key: &str) -> anyhow::Result<Option<u8>> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => {
            let raw = n
                .as_u64()
                .with_context(|| format!("field '{key}' must be an unsigned integer"))?;
            Ok(Some(u8::try_from(raw).with_context(|| {
                format!("field '{key}' out of u8 range")
            })?))
        }
        Some(_) => bail!("field '{key}' must be an integer"),
    }
}

fn req_u16(obj: &Map<String, Value>, key: &str) -> anyhow::Result<u16> {
    let raw = obj
        .get(key)
        .and_then(Value::as_u64)
        .with_context(|| format!("missing or non-integer field '{key}'"))?;
    u16::try_from(raw).with_context(|| format!("field '{key}' out of u16 range"))
}

fn req_u32(obj: &Map<String, Value>, key: &str) -> anyhow::Result<u32> {
    let raw = obj
        .get(key)
        .and_then(Value::as_u64)
        .with_context(|| format!("missing or non-integer field '{key}'"))?;
    u32::try_from(raw).with_context(|| format!("field '{key}' out of u32 range"))
}

fn opt_u32(obj: &Map<String, Value>, key: &str) -> anyhow::Result<Option<u32>> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => {
            let raw = n
                .as_u64()
                .with_context(|| format!("field '{key}' must be an unsigned integer"))?;
            Ok(Some(u32::try_from(raw).with_context(|| {
                format!("field '{key}' out of u32 range")
            })?))
        }
        Some(_) => bail!("field '{key}' must be an integer"),
    }
}

fn opt_u64(obj: &Map<String, Value>, key: &str) -> anyhow::Result<Option<u64>> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => {
            Ok(Some(n.as_u64().with_context(|| {
                format!("field '{key}' must be an unsigned integer")
            })?))
        }
        Some(_) => bail!("field '{key}' must be an integer"),
    }
}

fn camera_to_json(camera: &ReproCameraInfo) -> Value {
    let mut obj = Map::new();
    if let Some(name) = &camera.name {
        obj.insert("name".to_owned(), Value::String(name.clone()));
    }
    obj.insert(
        "protocol".to_owned(),
        Value::String(camera.protocol.as_str().to_owned()),
    );
    obj.insert("anonymized".to_owned(), Value::Bool(camera.anonymized));
    obj.insert(
        "credentials_stripped".to_owned(),
        Value::Bool(camera.credentials_stripped),
    );
    obj.insert(
        "base_host".to_owned(),
        Value::String(camera.base_host.clone()),
    );
    Value::Object(obj)
}

fn rtsp_to_json(rtsp: &RtspReproFixture) -> Value {
    let sessions = rtsp.sessions.iter().map(rtsp_session_to_json).collect();
    let mut obj = Map::new();
    obj.insert("sessions".to_owned(), Value::Array(sessions));
    Value::Object(obj)
}

fn rtsp_session_to_json(session: &RtspSessionFixture) -> Value {
    let mut obj = Map::new();
    if let Some(order) = &session.setup_order {
        obj.insert(
            "setup_order".to_owned(),
            Value::Array(order.iter().cloned().map(Value::String).collect()),
        );
    }
    obj.insert(
        "responses".to_owned(),
        Value::Array(
            session
                .responses
                .iter()
                .map(rtsp_response_to_json)
                .collect(),
        ),
    );
    obj.insert(
        "rtp_packets".to_owned(),
        Value::Array(session.rtp_packets.iter().map(rtp_packet_to_json).collect()),
    );
    Value::Object(obj)
}

fn rtsp_response_to_json(resp: &RtspResponseFixture) -> Value {
    let mut obj = Map::new();
    if let Some(method) = &resp.method {
        obj.insert("method".to_owned(), Value::String(method.clone()));
    }
    if let Some(uri) = &resp.uri_contains {
        obj.insert("uri_contains".to_owned(), Value::String(uri.clone()));
    }
    obj.insert("status".to_owned(), Value::from(resp.status));
    obj.insert("reason".to_owned(), Value::String(resp.reason.clone()));
    obj.insert("headers".to_owned(), headers_to_json(&resp.headers));
    obj.insert("body".to_owned(), Value::String(resp.body.clone()));
    Value::Object(obj)
}

fn rtp_packet_to_json(pkt: &RtpPacketFixture) -> Value {
    let mut obj = Map::new();
    if let Some(ch) = pkt.channel {
        obj.insert("channel".to_owned(), Value::from(ch));
    }
    obj.insert("track".to_owned(), Value::String(pkt.track.clone()));
    obj.insert("marker".to_owned(), Value::Bool(pkt.marker));
    obj.insert("payload_type".to_owned(), Value::from(pkt.payload_type));
    obj.insert("sequence".to_owned(), Value::from(pkt.sequence));
    obj.insert("timestamp".to_owned(), Value::from(pkt.timestamp));
    obj.insert("ssrc".to_owned(), Value::from(pkt.ssrc));
    if let Some(delay) = pkt.delay_ms {
        obj.insert("delay_ms".to_owned(), Value::from(delay));
    }
    obj.insert(
        "payload_base64".to_owned(),
        Value::String(pkt.payload_base64.clone()),
    );
    Value::Object(obj)
}

fn http_to_json(http: &HttpReproFixture) -> Value {
    let exchanges = http.exchanges.iter().map(http_exchange_to_json).collect();
    let mut obj = Map::new();
    obj.insert("exchanges".to_owned(), Value::Array(exchanges));
    Value::Object(obj)
}

fn http_exchange_to_json(ex: &HttpExchangeFixture) -> Value {
    let mut obj = Map::new();
    insert_opt_str(&mut obj, "match_method", ex.match_method.as_deref());
    insert_opt_str(
        &mut obj,
        "match_path_contains",
        ex.match_path_contains.as_deref(),
    );
    insert_opt_str(
        &mut obj,
        "match_soap_action",
        ex.match_soap_action.as_deref(),
    );
    insert_opt_str(
        &mut obj,
        "match_body_contains",
        ex.match_body_contains.as_deref(),
    );
    obj.insert("status".to_owned(), Value::from(ex.status));
    insert_opt_str(&mut obj, "status_reason", ex.status_reason.as_deref());
    obj.insert("headers".to_owned(), headers_to_json(&ex.headers));
    insert_opt_str(&mut obj, "body_text", ex.body_text.as_deref());
    insert_opt_str(&mut obj, "body_base64", ex.body_base64.as_deref());
    if let Some(delay) = ex.delay_ms {
        obj.insert("delay_ms".to_owned(), Value::from(delay));
    }
    Value::Object(obj)
}

fn reo_to_json(reo: &ReoProtoReproFixture) -> Value {
    let frames = reo.frames.iter().map(reo_frame_to_json).collect();
    let mut obj = Map::new();
    obj.insert("frames".to_owned(), Value::Array(frames));
    Value::Object(obj)
}

fn reo_frame_to_json(frame: &ReoFrameFixture) -> Value {
    let mut obj = Map::new();
    if let Some(expected) = frame.expected_msg_id {
        obj.insert("expected_msg_id".to_owned(), Value::from(expected));
    }
    obj.insert(
        "response_msg_id".to_owned(),
        Value::from(frame.response_msg_id),
    );
    insert_opt_str(&mut obj, "payload_base64", frame.payload_base64.as_deref());
    insert_opt_str(&mut obj, "payload_xml", frame.payload_xml.as_deref());
    if let Some(ch) = frame.channel_id {
        obj.insert("channel_id".to_owned(), Value::from(ch));
    }
    if let Some(st) = frame.stream_type {
        obj.insert("stream_type".to_owned(), Value::from(st));
    }
    if let Some(delay) = frame.delay_ms {
        obj.insert("delay_ms".to_owned(), Value::from(delay));
    }
    Value::Object(obj)
}

fn insert_opt_str(obj: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        obj.insert(key.to_owned(), Value::String(value.to_owned()));
    }
}

fn headers_to_json(headers: &[(String, String)]) -> Value {
    Value::Array(
        headers
            .iter()
            .map(|(k, v)| Value::Array(vec![Value::String(k.clone()), Value::String(v.clone())]))
            .collect(),
    )
}

fn validate_rtsp(rtsp: &RtspReproFixture) -> anyhow::Result<()> {
    ensure!(
        rtsp.sessions.len() <= SESSIONS_MAX,
        "too many RTSP sessions"
    );
    for session in &rtsp.sessions {
        ensure!(
            session.responses.len() <= RESPONSES_PER_SESSION_MAX,
            "too many RTSP responses in session"
        );
        ensure!(
            session.rtp_packets.len() <= RTP_PACKETS_PER_SESSION_MAX,
            "too many RTP packets in session"
        );
        for response in &session.responses {
            ensure!(
                (100..=599).contains(&response.status),
                "invalid RTSP status code {}",
                response.status
            );
            validate_headers(&response.headers)?;
        }
        for packet in &session.rtp_packets {
            ensure!(
                !packet.track.is_empty(),
                "RTP packet track name must not be empty"
            );
            ensure!(
                packet.payload_type <= 127,
                "RTP payload_type {} exceeds 7-bit maximum",
                packet.payload_type
            );
            validate_delay(packet.delay_ms)?;
            let payload = decode_base64(&packet.payload_base64)?;
            ensure!(
                payload.len() <= RTP_PAYLOAD_BYTES_MAX,
                "RTP payload exceeds maximum size"
            );
        }
    }
    Ok(())
}

fn validate_http(http: &HttpReproFixture) -> anyhow::Result<()> {
    ensure!(
        http.exchanges.len() <= HTTP_EXCHANGES_MAX,
        "too many HTTP exchanges"
    );
    for exchange in &http.exchanges {
        ensure!(
            (100..=599).contains(&exchange.status),
            "invalid HTTP status code {}",
            exchange.status
        );
        ensure!(
            !(exchange.body_text.is_some() && exchange.body_base64.is_some()),
            "HTTP exchange cannot specify both body_text and body_base64"
        );
        validate_headers(&exchange.headers)?;
        validate_delay(exchange.delay_ms)?;
        if let Some(encoded) = &exchange.body_base64 {
            let decoded = decode_base64(encoded)?;
            ensure!(
                decoded.len() <= HTTP_BODY_BYTES_MAX,
                "HTTP response body exceeds limit"
            );
        }
    }
    Ok(())
}

fn validate_reo(reo: &ReoProtoReproFixture) -> anyhow::Result<()> {
    ensure!(
        reo.frames.len() <= REO_FRAMES_MAX,
        "too many reo_proto frames"
    );
    for frame in &reo.frames {
        ensure!(
            !(frame.payload_base64.is_some() && frame.payload_xml.is_some()),
            "reo_proto frame cannot specify both payload_base64 and payload_xml"
        );
        validate_delay(frame.delay_ms)?;
        if let Some(encoded) = &frame.payload_base64 {
            let decoded = decode_base64(encoded)?;
            ensure!(
                decoded.len() <= REO_BODY_BYTES_MAX,
                "reo_proto frame exceeds body limit"
            );
        }
    }
    Ok(())
}

fn validate_headers(headers: &[(String, String)]) -> anyhow::Result<()> {
    ensure!(headers.len() <= HEADERS_PER_MESSAGE_MAX, "too many headers");
    for (name, value) in headers {
        ensure!(
            !name.is_empty() && !name.contains(['\r', '\n', ':']),
            "invalid header name '{name}'"
        );
        ensure!(
            !value.contains(['\r', '\n']),
            "header '{name}' contains newline characters"
        );
    }
    Ok(())
}

fn validate_delay(delay_ms: Option<u64>) -> anyhow::Result<()> {
    if let Some(delay) = delay_ms {
        ensure!(
            delay <= DELAY_MS_MAX,
            "replay delay {delay} ms exceeds {DELAY_MS_MAX} ms"
        );
    }
    Ok(())
}

fn start_http_listener(
    bind: SocketAddr,
    http: HttpReproFixture,
    shared: Arc<ReplayShared>,
) -> anyhow::Result<EndpointHandle> {
    let listener =
        TcpListener::bind(bind).with_context(|| format!("unable to bind HTTP replay on {bind}"))?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    let worker = thread::Builder::new()
        .name("replay-http".to_owned())
        .spawn(move || serve_http(listener, http, shared))?;
    Ok(EndpointHandle {
        addr,
        worker: Some(worker),
    })
}

fn serve_http(listener: TcpListener, http: HttpReproFixture, shared: Arc<ReplayShared>) {
    let exchanges = Arc::new(http.exchanges);
    let consumed = Arc::new(Mutex::new(vec![false; exchanges.len()]));
    let mut children: Vec<JoinHandle<()>> = Vec::new();
    while !shared.stop.load(Ordering::Acquire) {
        children.retain(|h| !h.is_finished());
        match listener.accept() {
            Ok((mut stream, _)) => {
                if children.len() >= CONCURRENT_CONNECTIONS_MAX {
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                }
                let exchanges = Arc::clone(&exchanges);
                let consumed = Arc::clone(&consumed);
                let shared = Arc::clone(&shared);
                if let Ok(child) = thread::Builder::new()
                    .name("replay-http-conn".to_owned())
                    .spawn(move || {
                        handle_http_connection(&mut stream, &exchanges, &consumed, &shared);
                    })
                {
                    children.push(child);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(POLL_INTERVAL);
            }
            Err(error) => {
                tracing::warn!(%error, "HTTP replay listener stopped");
                break;
            }
        }
    }
    for child in children {
        let _ = child.join();
    }
}

fn handle_http_connection(
    stream: &mut TcpStream,
    exchanges: &[HttpExchangeFixture],
    consumed: &Mutex<Vec<bool>>,
    shared: &ReplayShared,
) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    loop {
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        let Ok(Some(req)) = read_http_request(stream, &shared.stop) else {
            return;
        };
        let connection_close = req
            .headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("connection") && v.eq_ignore_ascii_case("close"));
        shared.record(RecordedRequest {
            protocol: RecordedProtocol::Http,
            method: req.method.clone(),
            target: req.target.clone(),
            headers: req.headers.clone(),
            body: req.body.clone(),
        });
        let matched = {
            let mut guard = consumed.lock().unwrap_or_else(|p| p.into_inner());
            let selected = select_http_exchange(exchanges, &mut guard, &req, shared.loop_replay);
            if guard.iter().all(|done| *done) {
                shared.http_done.store(true, Ordering::Release);
            }
            selected
        };
        let (close_reply, ok) =
            write_http_response(stream, matched, connection_close, &shared.stop);
        if !ok || close_reply {
            let _ = stream.shutdown(Shutdown::Both);
            return;
        }
    }
}

struct ParsedHttpRequest {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn read_http_request(
    stream: &mut TcpStream,
    stop: &AtomicBool,
) -> anyhow::Result<Option<ParsedHttpRequest>> {
    let mut buf = Vec::new();
    let mut byte = [0_u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if stop.load(Ordering::Acquire) {
            return Ok(None);
        }
        match stream.read(&mut byte) {
            Ok(0) => return Ok(None),
            Ok(_) => {
                buf.push(byte[0]);
                ensure!(buf.len() <= HTTP_HEADER_BYTES_MAX, "HTTP headers too large");
            }
            Err(error) if is_timeout(&error) => continue,
            Err(error) => return Err(error.into()),
        }
    }
    let head = std::str::from_utf8(&buf)?;
    let mut lines = head.split("\r\n");
    let request_line = lines.next().context("empty HTTP request")?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().context("missing HTTP method")?.to_owned();
    let target = parts.next().context("missing HTTP target")?.to_owned();
    let mut headers = Vec::new();
    let mut content_length = 0_usize;
    for line in lines {
        if line.is_empty() {
            break;
        }
        let Some((name, val)) = line.split_once(':') else {
            bail!("malformed HTTP header line");
        };
        let name = name.trim().to_owned();
        let val = val.trim().to_owned();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = val.parse()?;
            ensure!(content_length <= HTTP_BODY_BYTES_MAX, "HTTP body too large");
        }
        headers.push((name, val));
    }
    let body = read_exact_interruptible(stream, content_length, stop)?;
    Ok(body.map(|body| ParsedHttpRequest {
        method,
        target,
        headers,
        body,
    }))
}

fn select_http_exchange<'a>(
    exchanges: &'a [HttpExchangeFixture],
    consumed: &mut [bool],
    req: &ParsedHttpRequest,
    loop_replay: bool,
) -> Option<&'a HttpExchangeFixture> {
    let body_text = String::from_utf8_lossy(&req.body);
    if let Some(idx) = exchanges
        .iter()
        .enumerate()
        .position(|(idx, ex)| !consumed[idx] && http_matches(ex, req, &body_text))
    {
        consumed[idx] = true;
        return Some(&exchanges[idx]);
    }
    if loop_replay {
        let mut reset_any = false;
        for (idx, ex) in exchanges.iter().enumerate() {
            if http_matches(ex, req, &body_text) {
                consumed[idx] = false;
                reset_any = true;
            }
        }
        if reset_any
            && let Some(idx) = exchanges
                .iter()
                .enumerate()
                .position(|(idx, ex)| !consumed[idx] && http_matches(ex, req, &body_text))
        {
            consumed[idx] = true;
            return Some(&exchanges[idx]);
        }
    }
    None
}

fn http_matches(ex: &HttpExchangeFixture, req: &ParsedHttpRequest, body_text: &str) -> bool {
    if let Some(method) = &ex.match_method
        && !req.method.eq_ignore_ascii_case(method)
    {
        return false;
    }
    if let Some(path) = &ex.match_path_contains
        && !req.target.contains(path.as_str())
    {
        return false;
    }
    if let Some(action) = &ex.match_soap_action
        && !soap_action_matches(&req.headers, action)
    {
        return false;
    }
    if let Some(needle) = &ex.match_body_contains
        && !body_text.contains(needle.as_str())
    {
        return false;
    }
    true
}

fn soap_action_matches(headers: &[(String, String)], expected: &str) -> bool {
    let expected_lower = expected.to_ascii_lowercase();
    for (name, val) in headers {
        if name.eq_ignore_ascii_case("soapaction") {
            let trimmed = val.trim().trim_matches('"').to_ascii_lowercase();
            if trimmed == expected_lower || trimmed.contains(&expected_lower) {
                return true;
            }
        }
        if name.eq_ignore_ascii_case("content-type")
            && let Some(idx) = val.to_ascii_lowercase().find("action=")
        {
            let rest = val[idx + 7..].trim();
            let token = rest
                .strip_prefix('"')
                .and_then(|s| s.split('"').next())
                .unwrap_or_else(|| rest.split(';').next().unwrap_or(rest));
            let lower = token.to_ascii_lowercase();
            if lower == expected_lower || lower.contains(&expected_lower) {
                return true;
            }
        }
    }
    false
}

fn write_http_response(
    stream: &mut TcpStream,
    matched: Option<&HttpExchangeFixture>,
    client_wants_close: bool,
    stop: &AtomicBool,
) -> (bool, bool) {
    let (status, reason, headers, body, delay) = matched.map_or_else(
        || {
            (
                404,
                "Not Found",
                &[][..],
                b"no matching HTTP replay exchange".to_vec(),
                None,
            )
        },
        |ex| {
            let body = ex.body_base64.as_ref().map_or_else(
                || {
                    ex.body_text
                        .as_deref()
                        .map(|s| s.as_bytes().to_vec())
                        .unwrap_or_default()
                },
                |b64| decode_base64(b64).unwrap_or_default(),
            );
            let reason = ex
                .status_reason
                .as_deref()
                .unwrap_or_else(|| default_reason(ex.status));
            (ex.status, reason, ex.headers.as_slice(), body, ex.delay_ms)
        },
    );
    if !sleep_interruptible(delay, stop) {
        return (true, false);
    }
    let mut has_len = false;
    let mut has_conn = false;
    let mut explicit_close = client_wants_close;
    let mut wire = format!("HTTP/1.1 {status} {reason}\r\n").into_bytes();
    for (name, val) in headers {
        if name.eq_ignore_ascii_case("content-length") {
            has_len = true;
        }
        if name.eq_ignore_ascii_case("connection") {
            has_conn = true;
            if val.eq_ignore_ascii_case("close") {
                explicit_close = true;
            }
        }
        wire.extend_from_slice(format!("{name}: {val}\r\n").as_bytes());
    }
    if !has_len {
        wire.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
    }
    if !has_conn && explicit_close {
        wire.extend_from_slice(b"Connection: close\r\n");
    }
    wire.extend_from_slice(b"\r\n");
    wire.extend_from_slice(&body);
    let ok = stream.write_all(&wire).is_ok() && stream.flush().is_ok();
    (explicit_close, ok)
}

fn start_rtsp_listener(
    bind: SocketAddr,
    rtsp: RtspReproFixture,
    shared: Arc<ReplayShared>,
) -> anyhow::Result<EndpointHandle> {
    let listener =
        TcpListener::bind(bind).with_context(|| format!("unable to bind RTSP replay on {bind}"))?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    let worker = thread::Builder::new()
        .name("replay-rtsp".to_owned())
        .spawn(move || serve_rtsp(listener, rtsp, shared))?;
    Ok(EndpointHandle {
        addr,
        worker: Some(worker),
    })
}

fn serve_rtsp(listener: TcpListener, rtsp: RtspReproFixture, shared: Arc<ReplayShared>) {
    let sessions = Arc::new(rtsp.sessions);
    let consumed = Arc::new(Mutex::new(vec![false; sessions.len()]));
    let mut children: Vec<JoinHandle<()>> = Vec::new();
    while !shared.stop.load(Ordering::Acquire) {
        children.retain(|h| !h.is_finished());
        match listener.accept() {
            Ok((stream, _)) => {
                if children.len() >= CONCURRENT_CONNECTIONS_MAX {
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                }
                let sessions = Arc::clone(&sessions);
                let consumed = Arc::clone(&consumed);
                let shared = Arc::clone(&shared);
                if let Ok(child) = thread::Builder::new()
                    .name("replay-rtsp-conn".to_owned())
                    .spawn(move || {
                        let _ = serve_rtsp_connection(stream, &sessions, &consumed, &shared);
                    })
                {
                    children.push(child);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(POLL_INTERVAL);
            }
            Err(error) => {
                tracing::warn!(%error, "RTSP replay listener stopped");
                break;
            }
        }
    }
    for child in children {
        let _ = child.join();
    }
}

enum TrackTransport {
    Tcp {
        channel: u8,
    },
    Udp {
        rtp: UdpSocket,
        _rtcp: UdpSocket,
        client_rtp: SocketAddr,
    },
}

struct RtspConnState {
    selected: Option<usize>,
    resp_consumed: Vec<bool>,
    tracks: Vec<(String, TrackTransport)>,
    session_id: Option<String>,
    setup_index: usize,
}

impl RtspConnState {
    const fn new() -> Self {
        Self {
            selected: None,
            resp_consumed: Vec::new(),
            tracks: Vec::new(),
            session_id: None,
            setup_index: 0,
        }
    }

    fn ensure_session(
        &mut self,
        sessions: &[RtspSessionFixture],
        consumed_sessions: &Mutex<Vec<bool>>,
        uri: &str,
        loop_replay: bool,
    ) -> anyhow::Result<usize> {
        if let Some(idx) = self.selected {
            return Ok(idx);
        }
        let idx = select_rtsp_session(sessions, consumed_sessions, uri, loop_replay)?;
        self.resp_consumed = vec![false; sessions[idx].responses.len()];
        self.selected = Some(idx);
        Ok(idx)
    }
}

fn serve_rtsp_connection(
    mut stream: TcpStream,
    sessions: &[RtspSessionFixture],
    consumed_sessions: &Mutex<Vec<bool>>,
    shared: &ReplayShared,
) -> anyhow::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut framer = RtspFramer::default();
    let mut state = RtspConnState::new();

    while !shared.stop.load(Ordering::Acquire) {
        let Some(req) = read_rtsp_request(&mut stream, &mut framer, &shared.stop)? else {
            return Ok(());
        };
        let uri = record_rtsp_request(&req, shared);
        if !handle_rtsp_request(
            &mut stream,
            &req,
            &uri,
            sessions,
            consumed_sessions,
            &mut state,
            shared,
        )? {
            return Ok(());
        }
    }
    Ok(())
}

fn record_rtsp_request(req: &msg::Request, shared: &ReplayShared) -> String {
    let uri = req
        .request_uri
        .as_ref()
        .map_or_else(|| "*".to_owned(), ToString::to_string);
    let headers = req
        .headers
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    shared.record(RecordedRequest {
        protocol: RecordedProtocol::Rtsp,
        method: req.method.to_string(),
        target: uri.clone(),
        headers,
        body: Vec::new(),
    });
    uri
}

fn handle_rtsp_request(
    stream: &mut TcpStream,
    req: &msg::Request,
    uri: &str,
    sessions: &[RtspSessionFixture],
    consumed_sessions: &Mutex<Vec<bool>>,
    state: &mut RtspConnState,
    shared: &ReplayShared,
) -> anyhow::Result<bool> {
    if req.method == msg::Method::OPTIONS {
        let matched = state.selected.and_then(|idx| {
            take_rtsp_response(
                &sessions[idx].responses,
                &mut state.resp_consumed,
                "OPTIONS",
                uri,
            )
        });
        write_rtsp_reply(stream, req, matched, &[], state.session_id.as_deref())?;
        return Ok(true);
    }
    let idx = state.ensure_session(sessions, consumed_sessions, uri, shared.loop_replay)?;
    let session = &sessions[idx];
    let method_str = req.method.to_string();
    let matched = take_rtsp_response(
        &session.responses,
        &mut state.resp_consumed,
        &method_str,
        uri,
    );
    if req.method == msg::Method::TEARDOWN {
        write_rtsp_reply(stream, req, matched, &[], state.session_id.as_deref())?;
        return Ok(false);
    }
    if req.method == msg::Method::SETUP {
        let track_name = resolve_setup_track(session, uri, state.setup_index);
        state.setup_index += 1;
        let (transport, transport_header) = negotiate_rtsp_transport(req, stream, matched)?;
        if state.session_id.is_none() {
            state.session_id =
                extract_fixture_session_id(matched).or_else(|| Some(DEFAULT_SESSION_ID.to_owned()));
        }
        state.tracks.push((track_name, transport));
        write_rtsp_reply(
            stream,
            req,
            matched,
            &[("Transport".to_owned(), transport_header)],
            state.session_id.as_deref(),
        )?;
        return Ok(true);
    }
    let status = matched.map_or(200, |r| r.status);
    write_rtsp_reply(stream, req, matched, &[], state.session_id.as_deref())?;
    if req.method == msg::Method::PLAY && (200..300).contains(&status) {
        emit_rtsp_packets(stream, &state.tracks, &session.rtp_packets, shared)?;
        mark_rtsp_done_if_complete(consumed_sessions, shared);
        let has_udp = state
            .tracks
            .iter()
            .any(|(_, t)| matches!(t, TrackTransport::Udp { .. }));
        if !has_udp {
            let _ = stream.shutdown(Shutdown::Write);
            return Ok(false);
        }
    }
    Ok(true)
}

fn mark_rtsp_done_if_complete(consumed_sessions: &Mutex<Vec<bool>>, shared: &ReplayShared) {
    if consumed_sessions
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .all(|done| *done)
    {
        shared.rtsp_done.store(true, Ordering::Release);
    }
}

fn read_rtsp_request(
    stream: &mut TcpStream,
    framer: &mut RtspFramer,
    stop: &AtomicBool,
) -> anyhow::Result<Option<msg::Request>> {
    let mut buf = [0_u8; 16 * 1024];
    let mut total = 0_usize;
    while !stop.load(Ordering::Acquire) {
        match stream.read(&mut buf) {
            Ok(0) => return Ok(None),
            Ok(n) => {
                total = total.saturating_add(n);
                ensure!(total <= RTSP_MESSAGE_BYTES_MAX, "RTSP request too large");
                for framed in framer.push(&buf[..n])? {
                    if let msg::Message::Request(req) = framed.message {
                        return Ok(Some(req));
                    }
                }
            }
            Err(error) if is_timeout(&error) => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(None)
}

fn select_rtsp_session(
    sessions: &[RtspSessionFixture],
    consumed: &Mutex<Vec<bool>>,
    uri: &str,
    loop_replay: bool,
) -> anyhow::Result<usize> {
    let mut guard = consumed.lock().unwrap_or_else(|p| p.into_inner());
    let find_match = |done: &[bool]| {
        sessions
            .iter()
            .enumerate()
            .find(|(idx, s)| {
                !done[*idx]
                    && s.responses.iter().any(|r| {
                        r.uri_contains
                            .as_deref()
                            .is_some_and(|needle| uri.contains(needle))
                    })
            })
            .or_else(|| sessions.iter().enumerate().find(|(idx, _)| !done[*idx]))
            .map(|(idx, _)| idx)
    };
    if let Some(idx) = find_match(&guard) {
        guard[idx] = true;
        return Ok(idx);
    }
    if loop_replay && !sessions.is_empty() {
        guard.fill(false);
        if let Some(idx) = find_match(&guard) {
            guard[idx] = true;
            return Ok(idx);
        }
    }
    bail!("no unconsumed RTSP session available for {uri}")
}

fn take_rtsp_response<'a>(
    responses: &'a [RtspResponseFixture],
    consumed: &mut [bool],
    method: &str,
    uri: &str,
) -> Option<&'a RtspResponseFixture> {
    let idx = responses.iter().enumerate().position(|(i, r)| {
        !consumed[i]
            && r.method
                .as_deref()
                .is_none_or(|m| m.eq_ignore_ascii_case(method))
            && r.uri_contains.as_deref().is_none_or(|u| uri.contains(u))
    })?;
    consumed[idx] = true;
    Some(&responses[idx])
}

fn resolve_setup_track(session: &RtspSessionFixture, uri: &str, setup_index: usize) -> String {
    if let Some(order) = &session.setup_order
        && let Some(track) = order.get(setup_index)
    {
        return track.clone();
    }
    let mut distinct = Vec::new();
    for pkt in &session.rtp_packets {
        if !distinct.contains(&pkt.track) {
            distinct.push(pkt.track.clone());
        }
    }
    if let Some(matched) = distinct.iter().find(|t| uri.contains(t.as_str())) {
        return matched.clone();
    }
    distinct
        .get(setup_index)
        .cloned()
        .unwrap_or_else(|| format!("trackID={setup_index}"))
}

fn negotiate_rtsp_transport(
    req: &msg::Request,
    stream: &TcpStream,
    matched: Option<&RtspResponseFixture>,
) -> anyhow::Result<(TrackTransport, String)> {
    let req_transport = req
        .headers
        .get("Transport")
        .map(ToString::to_string)
        .context("RTSP SETUP missing Transport header")?;
    let ssrc_part = matched.and_then(|resp| {
        resp.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("transport"))
            .and_then(|(_, v)| v.split(';').find(|p| p.trim().starts_with("ssrc=")))
            .map(|p| format!(";{}", p.trim()))
    });
    let ssrc = ssrc_part.as_deref().unwrap_or("");
    if let Some(ch) = req_transport
        .split(';')
        .find_map(|p| p.trim().strip_prefix("interleaved="))
    {
        let (rtp, rtcp) = ch.split_once('-').context("invalid interleaved pair")?;
        let rtp_ch: u8 = rtp.parse()?;
        let rtcp_ch: u8 = rtcp.parse()?;
        let hdr = format!("RTP/AVP/TCP;unicast;interleaved={rtp_ch}-{rtcp_ch}{ssrc}");
        return Ok((TrackTransport::Tcp { channel: rtp_ch }, hdr));
    }
    let ports = req_transport
        .split(';')
        .find_map(|p| p.trim().strip_prefix("client_port="))
        .context("RTSP SETUP lacks interleaved or client_port")?;
    let (client_rtp_port, client_rtcp_port) =
        ports.split_once('-').context("invalid client_port")?;
    let client_rtp_port: u16 = client_rtp_port.parse()?;
    let client_rtcp_port: u16 = client_rtcp_port.parse()?;
    let local_ip = stream.local_addr()?.ip();
    let peer_ip = stream.peer_addr()?.ip();
    let (rtp, rtcp) = bind_udp_pair(local_ip)?;
    let srv_port = rtp.local_addr()?.port();
    let hdr = format!(
        "RTP/AVP/UDP;unicast;client_port={client_rtp_port}-{client_rtcp_port};server_port={srv_port}-{};source={local_ip}{ssrc}",
        srv_port + 1
    );
    Ok((
        TrackTransport::Udp {
            rtp,
            _rtcp: rtcp,
            client_rtp: SocketAddr::new(peer_ip, client_rtp_port),
        },
        hdr,
    ))
}

fn bind_udp_pair(local_ip: IpAddr) -> anyhow::Result<(UdpSocket, UdpSocket)> {
    for _ in 0..100 {
        let rtp = UdpSocket::bind(SocketAddr::new(local_ip, 0))?;
        let rtp_port = rtp.local_addr()?.port();
        if let Some(rtcp_port) = rtp_port.checked_add(1)
            && let Ok(rtcp) = UdpSocket::bind(SocketAddr::new(local_ip, rtcp_port))
        {
            return Ok((rtp, rtcp));
        }
    }
    bail!("unable to allocate adjacent UDP RTP/RTCP ports")
}

fn extract_fixture_session_id(matched: Option<&RtspResponseFixture>) -> Option<String> {
    matched.and_then(|resp| {
        resp.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("session"))
            .map(|(_, v)| v.split(';').next().unwrap_or(v).trim().to_owned())
    })
}

fn write_rtsp_reply(
    stream: &mut TcpStream,
    req: &msg::Request,
    matched: Option<&RtspResponseFixture>,
    override_headers: &[(String, String)],
    session_id: Option<&str>,
) -> anyhow::Result<()> {
    let (status, reason, headers, body) = match matched {
        Some(r) => (
            r.status,
            r.reason.as_str(),
            r.headers.as_slice(),
            r.body.as_str(),
        ),
        None if req.method == msg::Method::OPTIONS => (200, "OK", &[][..], ""),
        None if req.method == msg::Method::TEARDOWN => (200, "OK", &[][..], ""),
        None => (404, "Not Found", &[][..], ""),
    };
    let mut out = format!("RTSP/1.0 {status} {reason}\r\n").into_bytes();
    if let Some(cseq) = req.headers.get("CSeq") {
        out.extend_from_slice(format!("CSeq: {cseq}\r\n").as_bytes());
    }
    let mut wrote_session = false;
    for (name, val) in headers {
        if name.eq_ignore_ascii_case("cseq")
            || name.eq_ignore_ascii_case("content-length")
            || override_headers
                .iter()
                .any(|(ov_k, _)| ov_k.eq_ignore_ascii_case(name))
        {
            continue;
        }
        if name.eq_ignore_ascii_case("session") {
            wrote_session = true;
        }
        out.extend_from_slice(format!("{name}: {val}\r\n").as_bytes());
    }
    for (name, val) in override_headers {
        out.extend_from_slice(format!("{name}: {val}\r\n").as_bytes());
    }
    if !wrote_session && let Some(sid) = session_id {
        out.extend_from_slice(format!("Session: {sid}\r\n").as_bytes());
    }
    if req.method == msg::Method::OPTIONS && matched.is_none() {
        out.extend_from_slice(b"Public: OPTIONS, DESCRIBE, SETUP, PLAY, TEARDOWN\r\n");
    }
    if !body.is_empty() {
        out.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body.as_bytes());
    stream.write_all(&out)?;
    stream.flush()?;
    Ok(())
}

fn emit_rtsp_packets(
    stream: &mut TcpStream,
    tracks: &[(String, TrackTransport)],
    packets: &[RtpPacketFixture],
    shared: &ReplayShared,
) -> anyhow::Result<()> {
    loop {
        for pkt in packets {
            if !sleep_interruptible(pkt.delay_ms, &shared.stop) {
                return Ok(());
            }
            let rtp = build_rtp_packet(pkt)?;
            let transport = tracks
                .iter()
                .find(|(name, _)| name == &pkt.track)
                .or_else(|| (tracks.len() == 1).then(|| &tracks[0]))
                .map(|(_, t)| t);
            match transport {
                Some(TrackTransport::Tcp { channel }) => {
                    send_interleaved_rtp(stream, *channel, &rtp)?;
                }
                Some(TrackTransport::Udp {
                    rtp: sock,
                    client_rtp,
                    ..
                }) => {
                    sock.send_to(&rtp, client_rtp)?;
                }
                None => {
                    let ch = pkt.channel.unwrap_or(0);
                    send_interleaved_rtp(stream, ch, &rtp)?;
                }
            }
        }
        if !shared.loop_replay || packets.is_empty() {
            break;
        }
    }
    stream.flush()?;
    Ok(())
}

fn build_rtp_packet(pkt: &RtpPacketFixture) -> anyhow::Result<Vec<u8>> {
    let payload = decode_base64(&pkt.payload_base64)?;
    let mut rtp = Vec::with_capacity(12 + payload.len());
    rtp.push(0x80);
    rtp.push((u8::from(pkt.marker) << 7) | (pkt.payload_type & 0x7f));
    rtp.extend_from_slice(&pkt.sequence.to_be_bytes());
    rtp.extend_from_slice(&pkt.timestamp.to_be_bytes());
    rtp.extend_from_slice(&pkt.ssrc.to_be_bytes());
    assert_eq!(rtp.len(), 12, "RTP fixed header must be 12 bytes");
    rtp.extend_from_slice(&payload);
    Ok(rtp)
}

fn send_interleaved_rtp(stream: &mut TcpStream, channel: u8, rtp: &[u8]) -> anyhow::Result<()> {
    let len = u16::try_from(rtp.len()).context("RTP packet exceeds interleaved u16 length")?;
    let mut frame = Vec::with_capacity(4 + rtp.len());
    frame.push(b'$');
    frame.push(channel);
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(rtp);
    stream.write_all(&frame)?;
    Ok(())
}

fn start_reo_listener(
    bind: SocketAddr,
    reo: ReoProtoReproFixture,
    shared: Arc<ReplayShared>,
) -> anyhow::Result<EndpointHandle> {
    let listener = TcpListener::bind(bind)
        .with_context(|| format!("unable to bind reo_proto replay on {bind}"))?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    let worker = thread::Builder::new()
        .name("replay-reo".to_owned())
        .spawn(move || serve_reo(listener, reo, shared))?;
    Ok(EndpointHandle {
        addr,
        worker: Some(worker),
    })
}

fn serve_reo(listener: TcpListener, reo: ReoProtoReproFixture, shared: Arc<ReplayShared>) {
    let frames = Arc::new(reo.frames);
    let mut children: Vec<JoinHandle<()>> = Vec::new();
    while !shared.stop.load(Ordering::Acquire) {
        children.retain(|h| !h.is_finished());
        match listener.accept() {
            Ok((stream, _)) => {
                if children.len() >= CONCURRENT_CONNECTIONS_MAX {
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                }
                let frames = Arc::clone(&frames);
                let shared = Arc::clone(&shared);
                if let Ok(child) = thread::Builder::new()
                    .name("replay-reo-conn".to_owned())
                    .spawn(move || {
                        let _ = serve_reo_connection(stream, &frames, &shared);
                    })
                {
                    children.push(child);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(POLL_INTERVAL);
            }
            Err(error) => {
                tracing::warn!(%error, "reo_proto replay listener stopped");
                break;
            }
        }
    }
    for child in children {
        let _ = child.join();
    }
}

fn serve_reo_connection(
    mut stream: TcpStream,
    frames: &[ReoFrameFixture],
    shared: &ReplayShared,
) -> anyhow::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut read_buf = ReadBuffer::new();
    let mut negotiated = EncryptionMode::None;

    loop {
        for frame in frames {
            if shared.stop.load(Ordering::Acquire) {
                return Ok(());
            }
            if let Some(expected_id) = frame.expected_msg_id
                && !wait_for_reo_msg(&mut stream, &mut read_buf, expected_id, shared)?
            {
                return Ok(());
            }
            if !sleep_interruptible(frame.delay_ms, &shared.stop) {
                return Ok(());
            }
            if let Some(xml) = &frame.payload_xml
                && frame.response_msg_id == COMMAND_LOGIN
                && xml.contains("<Encryption")
                && xml.contains(">bc<")
            {
                negotiated = EncryptionMode::BcEncrypt;
            }
            send_reo_frame(&mut stream, frame, negotiated)?;
        }
        stream.flush()?;
        shared.reo_done.store(true, Ordering::Release);
        if !shared.loop_replay {
            wait_for_tcp_eof(&mut stream, &shared.stop);
            return Ok(());
        }
    }
}

fn wait_for_reo_msg(
    stream: &mut TcpStream,
    read_buf: &mut ReadBuffer,
    expected_id: u32,
    shared: &ReplayShared,
) -> anyhow::Result<bool> {
    let mut chunk = [0_u8; 64 * 1024];
    while !shared.stop.load(Ordering::Acquire) {
        if let Some(raw) = read_buf
            .try_parse_message()
            .map_err(|e| anyhow::anyhow!(e.to_string()))?
        {
            shared.record(RecordedRequest {
                protocol: RecordedProtocol::ReoProto,
                method: format!("msg_id:{}", raw.header.msg_id),
                target: format!(
                    "ch={}:stream={}",
                    raw.header.channel_id(),
                    raw.header.stream_type_id()
                ),
                headers: vec![
                    ("bc_class".to_owned(), raw.header.bc_class().to_string()),
                    (
                        "response_code".to_owned(),
                        raw.header.response_code().to_string(),
                    ),
                ],
                body: raw.body,
            });
            if raw.header.msg_id == expected_id {
                return Ok(true);
            }
            if raw.header.msg_id == COMMAND_PING {
                let ping = PacketHeader {
                    msg_id: COMMAND_PING,
                    body_len: 0,
                    encryption_offset: raw.header.encryption_offset,
                    status_class: make_status(BC_CLASS_MODERN_EXT, 200),
                    extension: Some(0),
                };
                let mut hdr = [0_u8; HEADER_LEN_EXTENDED];
                let len = ping.serialize(&mut hdr);
                stream.write_all(&hdr[..len])?;
            }
            continue;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Ok(false),
            Ok(n) => read_buf.extend(&chunk[..n]),
            Err(error) if is_timeout(&error) => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(false)
}

fn send_reo_frame(
    stream: &mut TcpStream,
    frame: &ReoFrameFixture,
    negotiated: EncryptionMode,
) -> anyhow::Result<()> {
    let enc_offset =
        u32::from(frame.channel_id.unwrap_or(0)) | (u32::from(frame.stream_type.unwrap_or(0)) << 8);
    let (body, status_class, extension) = if let Some(b64) = &frame.payload_base64 {
        let body = decode_base64(b64)?;
        let class = if frame.response_msg_id == COMMAND_STREAM && !body.starts_with(b"<") {
            BC_CLASS_LEGACY
        } else {
            BC_CLASS_MODERN_EXT
        };
        let ext = (class == BC_CLASS_MODERN_EXT).then_some(0);
        (body, make_status(class, 0), ext)
    } else if let Some(xml) = &frame.payload_xml {
        let mut body = xml.as_bytes().to_vec();
        if frame.response_msg_id == COMMAND_LOGIN {
            encryption::bc_xor(&mut body, 0);
            if xml.contains("<Encryption") {
                let code = if xml.contains(">bc<") { 0xDD01 } else { 0xDD00 };
                (body, make_status(BC_CLASS_LEGACY, code), None)
            } else {
                (body, make_status(BC_CLASS_MODERN_EXT, 0), Some(0))
            }
        } else {
            if matches!(negotiated, EncryptionMode::BcEncrypt) {
                encryption::bc_xor(&mut body, frame.channel_id.unwrap_or(0));
            }
            (body, make_status(BC_CLASS_MODERN_EXT, 0), Some(0))
        }
    } else {
        (Vec::new(), make_status(BC_CLASS_MODERN_EXT, 0), Some(0))
    };
    let header = PacketHeader {
        msg_id: frame.response_msg_id,
        body_len: u32::try_from(body.len()).context("reo_proto body length overflow")?,
        encryption_offset: enc_offset,
        status_class,
        extension,
    };
    let mut hdr_buf = [0_u8; HEADER_LEN_EXTENDED];
    let hdr_len = header.serialize(&mut hdr_buf);
    let mut packet = Vec::with_capacity(hdr_len + body.len());
    packet.extend_from_slice(&hdr_buf[..hdr_len]);
    packet.extend_from_slice(&body);
    stream.write_all(&packet)?;
    Ok(())
}

fn wait_for_tcp_eof(stream: &mut TcpStream, stop: &AtomicBool) {
    let mut buf = [0_u8; 1024];
    while !stop.load(Ordering::Acquire) {
        match stream.read(&mut buf) {
            Ok(0) => return,
            Ok(_) => {}
            Err(error) if is_timeout(&error) => continue,
            Err(_) => return,
        }
    }
}

fn read_exact_interruptible(
    stream: &mut TcpStream,
    len: usize,
    stop: &AtomicBool,
) -> anyhow::Result<Option<Vec<u8>>> {
    let mut body = vec![0_u8; len];
    let mut offset = 0_usize;
    while offset < len {
        if stop.load(Ordering::Acquire) {
            return Ok(None);
        }
        match stream.read(&mut body[offset..]) {
            Ok(0) => return Ok(None),
            Ok(n) => offset += n,
            Err(error) if is_timeout(&error) => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(Some(body))
}

fn sleep_interruptible(delay_ms: Option<u64>, stop: &AtomicBool) -> bool {
    let Some(ms) = delay_ms.filter(|ms| *ms > 0) else {
        return !stop.load(Ordering::Acquire);
    };
    let deadline = Instant::now() + Duration::from_millis(ms.min(DELAY_MS_MAX));
    while Instant::now() < deadline {
        if stop.load(Ordering::Acquire) {
            return false;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        thread::sleep(remaining.min(POLL_INTERVAL));
    }
    !stop.load(Ordering::Acquire)
}

fn is_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

const fn default_reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Replay",
    }
}

/// Encodes arbitrary bytes as standard padded base64.
pub fn encode_base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(*chunk.get(1).unwrap_or(&0));
        let b2 = u32::from(*chunk.get(2).unwrap_or(&0));
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((triple >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(triple & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Decodes standard padded base64 bytes, rejecting whitespace or malformed characters.
///
/// # Errors
/// Returns an error when the input length is not a multiple of 4 or contains invalid symbols.
pub fn decode_base64(input: &str) -> anyhow::Result<Vec<u8>> {
    ensure!(input.len().is_multiple_of(4), "invalid base64 length");
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity((bytes.len() / 4) * 3);
    for (idx, chunk) in bytes.as_chunks::<4>().0.iter().enumerate() {
        let last = (idx + 1) * 4 == bytes.len();
        let v0 = b64_val(chunk[0])?;
        let v1 = b64_val(chunk[1])?;
        let pad2 = chunk[2] == b'=';
        let pad3 = chunk[3] == b'=';
        ensure!(!(pad2 && !pad3), "invalid base64 padding");
        ensure!(!((pad2 || pad3) && !last), "unexpected base64 padding");
        let v2 = if pad2 { 0 } else { b64_val(chunk[2])? };
        let v3 = if pad3 { 0 } else { b64_val(chunk[3])? };
        let triple =
            (u32::from(v0) << 18) | (u32::from(v1) << 12) | (u32::from(v2) << 6) | u32::from(v3);
        out.push((triple >> 16) as u8);
        if !pad2 {
            out.push((triple >> 8) as u8);
        }
        if !pad3 {
            out.push(triple as u8);
        }
    }
    Ok(out)
}

fn b64_val(byte: u8) -> anyhow::Result<u8> {
    match byte {
        b'A'..=b'Z' => Ok(byte - b'A'),
        b'a'..=b'z' => Ok(byte - b'a' + 26),
        b'0'..=b'9' => Ok(byte - b'0' + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => bail!("invalid base64 character"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TempFixtureFile(PathBuf);

    impl TempFixtureFile {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "keeppeek-replay-test-{}-{}-{name}.json",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
            Self(path)
        }
    }

    impl Drop for TempFixtureFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn minimal_fixture_json(schema_version: &str) -> String {
        serde_json::json!({
            "schema_version": schema_version,
            "camera": {
                "name": "cam-1",
                "protocol": "rtsp",
                "anonymized": true,
                "credentials_stripped": true,
                "base_host": "192.0.2.10"
            },
            "sharing_safety_report": {
                "safe_to_share_publicly": true,
                "warnings": []
            }
        })
        .to_string()
    }

    #[test]
    fn base64_roundtrip_and_rejection() {
        for raw in [&b""[..], b"a", b"ab", b"abc", b"hello camera!"] {
            let encoded = encode_base64(raw);
            assert_eq!(decode_base64(&encoded).unwrap(), raw);
        }
        assert!(decode_base64("abc").is_err());
        assert!(decode_base64("ab=d").is_err());
        assert!(decode_base64("ab==").is_ok());
        assert!(decode_base64("ab==cd==").is_err());
        assert!(decode_base64("!!!!").is_err());
    }

    #[test]
    fn schema_validation_rejects_wrong_version_and_unknown_protocol() {
        let bad_version = minimal_fixture_json("keeppeek.camera-repro.v99");
        assert!(CameraReproFixture::from_json_str(&bad_version).is_err());

        let bad_proto = serde_json::json!({
            "schema_version": REPRO_SCHEMA_VERSION,
            "camera": {
                "protocol": "proprietary_magic",
                "anonymized": true,
                "credentials_stripped": true,
                "base_host": "192.0.2.10"
            }
        })
        .to_string();
        assert!(CameraReproFixture::from_json_str(&bad_proto).is_err());
    }

    #[test]
    fn schema_validation_rejects_oversized_and_invalid_bounds() {
        let oversized = vec![b' '; REPRO_FIXTURE_BYTES_MAX + 1];
        assert!(CameraReproFixture::from_json_bytes(&oversized).is_err());

        let file = TempFixtureFile::new("oversized");
        File::create(&file.0)
            .unwrap()
            .set_len((REPRO_FIXTURE_BYTES_MAX + 1) as u64)
            .unwrap();
        assert!(CameraReproFixture::from_file(&file.0).is_err());

        let valid = minimal_fixture_json(REPRO_SCHEMA_VERSION);
        let mut parsed = CameraReproFixture::from_json_str(&valid).unwrap();
        parsed.rtsp = Some(RtspReproFixture {
            sessions: vec![RtspSessionFixture {
                setup_order: None,
                responses: vec![],
                rtp_packets: vec![RtpPacketFixture {
                    channel: Some(0),
                    track: "video".to_owned(),
                    marker: true,
                    payload_type: 128,
                    sequence: 1,
                    timestamp: 90_000,
                    ssrc: 1,
                    delay_ms: None,
                    payload_base64: encode_base64(&[0x65]),
                }],
            }],
        });
        assert!(parsed.validate().is_err());
    }

    #[test]
    fn fixture_roundtrip_ignores_sharing_safety_report_metadata() {
        let raw = minimal_fixture_json(REPRO_SCHEMA_VERSION);
        let fixture = CameraReproFixture::from_json_str(&raw).unwrap();
        assert_eq!(fixture.camera.name.as_deref(), Some("cam-1"));
        assert_eq!(fixture.camera.protocol, ReproProtocol::Rtsp);
        let roundtrip =
            CameraReproFixture::from_json_str(&fixture.to_json_string().unwrap()).unwrap();
        assert_eq!(roundtrip, fixture);
    }
}
