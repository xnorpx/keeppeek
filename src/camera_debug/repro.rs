//! Exports local camera debug captures into sanitized `keeppeek.camera-repro.v1` fixtures.
//!
//! Converts recorded RTSP/RTP, HTTP/ONVIF/ISAPI, and Reolink Baichuan transcripts
//! into deterministic offline replay fixtures while stripping credentials,
//! anonymizing network identifiers to RFC 5737 / `.example.invalid`, trimming
//! media payloads, and emitting a strict [`SharingSafetyReport`].

use crate::camera_debug::{
    bitstream::CapturedRtpPacket,
    protocols::{
        CaptureProtocol, HttpExchangeTranscript, ProtocolTranscript, ReoFrameTranscript,
        RtspTranscript,
    },
    store::CaptureStore,
};
use anyhow::{Context, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use std::{fmt, net::Ipv4Addr, path::Path};
use zeroize::Zeroize;

/// Schema identifier expected by `ReplayCameraServer`.
pub const REPRO_SCHEMA_VERSION: &str = "keeppeek.camera-repro.v1";
/// Maximum serialized fixture size permitted by replay consumers (32 MiB).
pub const MAX_REPRO_FIXTURE_BYTES: usize = 32 * 1024 * 1024;
/// RFC 5737 TEST-NET-1 replacement address used when anonymizing camera hosts.
pub const ANONYMIZED_CAMERA_IP: &str = "192.0.2.10";
/// RFC 5737 TEST-NET-1 replacement hostname used when anonymizing camera URIs.
pub const ANONYMIZED_HOST: &str = "camera-1.example.invalid";
const MINIMAL_SLICE_PACKET_CAP: usize = 32;
const SYNTHETIC_PAYLOAD_MAX_BYTES: usize = 64;

/// Controls how captured media payloads are trimmed during repro export.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrimMediaMode {
    #[default]
    HeadersAndMinimalFailingSlice,
    SyntheticPayloads,
    KeepRawFrames,
}

/// Half-open `[start, end)` index range for slicing packets or NALUs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexRange {
    pub start: usize,
    pub end: usize,
}

/// Options governing sanitization, anonymization, and media slicing on repro export.
#[derive(Clone)]
pub struct ExportReproOptions {
    pub strip_credentials: bool,
    pub anonymize_network: bool,
    pub trim_media: TrimMediaMode,
    pub packet_range: Option<IndexRange>,
    pub nalu_range: Option<IndexRange>,
    pub extra_secrets: Vec<String>,
}

impl fmt::Debug for ExportReproOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExportReproOptions")
            .field("strip_credentials", &self.strip_credentials)
            .field("anonymize_network", &self.anonymize_network)
            .field("trim_media", &self.trim_media)
            .field("packet_range", &self.packet_range)
            .field("nalu_range", &self.nalu_range)
            .field("extra_secrets_count", &self.extra_secrets.len())
            .finish()
    }
}

impl Drop for ExportReproOptions {
    fn drop(&mut self) {
        for secret in &mut self.extra_secrets {
            secret.zeroize();
        }
    }
}

impl Default for ExportReproOptions {
    fn default() -> Self {
        Self {
            strip_credentials: true,
            anonymize_network: true,
            trim_media: TrimMediaMode::HeadersAndMinimalFailingSlice,
            packet_range: None,
            nalu_range: None,
            extra_secrets: Vec::new(),
        }
    }
}

/// Exported `keeppeek.camera-repro.v1` fixture compatible with `ReplayCameraServer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CameraReproFixture {
    pub schema_version: String,
    pub camera: ReproCamera,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtsp: Option<ReproRtsp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<ReproHttp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reo_proto: Option<ReproReoProto>,
    pub sharing_safety_report: SharingSafetyReport,
}

/// Camera identity section inside [`CameraReproFixture`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReproCamera {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub protocol: CaptureProtocol,
    pub anonymized: bool,
    pub credentials_stripped: bool,
    pub base_host: String,
}

/// RTSP sessions inside [`CameraReproFixture`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReproRtsp {
    pub sessions: Vec<ReproRtspSession>,
}

/// Single RTSP control + RTP replay session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReproRtspSession {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_order: Option<Vec<String>>,
    pub responses: Vec<ReproRtspResponse>,
    pub rtp_packets: Vec<ReproRtpPacket>,
}

/// Scripted RTSP response entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReproRtspResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri_contains: Option<String>,
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// Scripted RTP packet entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReproRtpPacket {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<u8>,
    pub track: String,
    pub marker: bool,
    pub payload_type: u8,
    pub sequence: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<u64>,
    pub payload_base64: String,
}

/// HTTP/ONVIF/ISAPI/Reolink exchanges inside [`CameraReproFixture`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReproHttp {
    pub exchanges: Vec<ReproHttpExchange>,
}

/// Single deterministic HTTP exchange.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReproHttpExchange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_path_contains: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_soap_action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_body_contains: Option<String>,
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_reason: Option<String>,
    pub headers: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<u64>,
}

/// Reolink Baichuan frames inside [`CameraReproFixture`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReproReoProto {
    pub frames: Vec<ReproReoFrame>,
}

/// Single scripted Reolink Baichuan frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReproReoFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_msg_id: Option<u32>,
    pub response_msg_id: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_xml: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_type: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<u64>,
}

/// Safety verification report attached to every exported repro fixture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharingSafetyReport {
    pub safe_to_share_publicly: bool,
    pub sanitized: bool,
    pub strip_credentials: bool,
    pub anonymize_network: bool,
    pub trim_media: TrimMediaMode,
    pub warnings: Vec<String>,
}

/// Loads `capture_id` from `store` and exports a [`CameraReproFixture`].
///
/// # Errors
/// Returns an error if `capture_id` is invalid, its transcript is missing or corrupt,
/// index ranges are inverted, or the serialized fixture exceeds 32 MiB.
pub fn export_repro_fixture(
    store: &CaptureStore,
    capture_id: &str,
    options: &ExportReproOptions,
) -> anyhow::Result<CameraReproFixture> {
    let dir = store.capture_dir(capture_id)?;
    let transcript_path = dir.join("protocol_transcript.json");
    let raw = std::fs::read(&transcript_path)
        .with_context(|| format!("missing transcript {}", transcript_path.display()))?;
    let transcript: ProtocolTranscript = serde_json::from_slice(&raw)
        .with_context(|| format!("invalid transcript {}", transcript_path.display()))?;
    export_repro_fixture_from_transcript(&transcript, options)
}

/// Writes an exported [`CameraReproFixture`] to `output_path` atomically.
///
/// # Errors
/// Returns an error if export fails, the fixture exceeds 32 MiB, or writing fails.
pub fn export_repro_to_file(
    store: &CaptureStore,
    capture_id: &str,
    output_path: impl AsRef<Path>,
    options: &ExportReproOptions,
) -> anyhow::Result<CameraReproFixture> {
    let fixture = export_repro_fixture(store, capture_id, options)?;
    let json = serde_json::to_vec_pretty(&fixture)?;
    ensure!(
        json.len() <= MAX_REPRO_FIXTURE_BYTES,
        "repro fixture size {} exceeds {MAX_REPRO_FIXTURE_BYTES}",
        json.len()
    );
    std::fs::write(output_path, json)?;
    Ok(fixture)
}

/// Exports a [`CameraReproFixture`] directly from an in-memory [`ProtocolTranscript`].
///
/// # Errors
/// Returns an error if `packet_range` or `nalu_range` is invalid or fixture exceeds 32 MiB.
pub fn export_repro_fixture_from_transcript(
    transcript: &ProtocolTranscript,
    options: &ExportReproOptions,
) -> anyhow::Result<CameraReproFixture> {
    validate_range(options.packet_range, "packet_range")?;
    validate_range(options.nalu_range, "nalu_range")?;

    let scrubber = TextScrubber {
        strip_credentials: options.strip_credentials,
        anonymize_network: options.anonymize_network,
        source_host: &transcript.camera_host,
        extra_secrets: &options.extra_secrets,
    };

    let base_host = if options.anonymize_network {
        ANONYMIZED_HOST.to_owned()
    } else {
        transcript.camera_host.clone()
    };
    let camera_name = if options.anonymize_network {
        Some("camera-1".to_owned())
    } else {
        Some(transcript.camera_id.clone())
    };
    let rtsp = transcript
        .rtsp
        .as_ref()
        .map(|rtsp| build_rtsp_section(rtsp, options, &scrubber));
    let http = (!transcript.http.is_empty()).then(|| ReproHttp {
        exchanges: transcript
            .http
            .iter()
            .map(|ex| build_http_exchange(ex, &scrubber))
            .collect(),
    });
    let reo_proto = (!transcript.reo_proto.is_empty()).then(|| ReproReoProto {
        frames: build_reo_frames(&transcript.reo_proto, options, &scrubber),
    });

    let mut fixture = CameraReproFixture {
        schema_version: REPRO_SCHEMA_VERSION.to_owned(),
        camera: ReproCamera {
            name: camera_name,
            protocol: transcript.protocol,
            anonymized: options.anonymize_network,
            credentials_stripped: options.strip_credentials,
            base_host,
        },
        rtsp,
        http,
        reo_proto,
        sharing_safety_report: build_safety_report(options, Vec::new()),
    };
    let warnings = audit_exported_fixture(&fixture, options)?;
    fixture.sharing_safety_report = build_safety_report(options, warnings);
    Ok(fixture)
}

fn validate_range(range: Option<IndexRange>, name: &str) -> anyhow::Result<()> {
    if let Some(r) = range {
        ensure!(
            r.start <= r.end,
            "{name} start ({}) must be <= end ({})",
            r.start,
            r.end
        );
    }
    Ok(())
}

fn build_rtsp_section(
    rtsp: &RtspTranscript,
    options: &ExportReproOptions,
    scrubber: &TextScrubber<'_>,
) -> ReproRtsp {
    let responses = rtsp
        .exchanges
        .iter()
        .map(|ex| ReproRtspResponse {
            method: Some(ex.method.clone()),
            uri_contains: extract_path_fragment(&ex.uri),
            status: ex.status,
            reason: ex.reason.clone(),
            headers: scrub_headers(&ex.response_headers, scrubber),
            body: scrubber.scrub_text(&ex.response_body),
        })
        .collect();
    let sliced = slice_rtp_packets(&rtsp.rtp_packets, options);
    let rtp_packets = sliced
        .into_iter()
        .map(|pkt| transform_rtp_packet(pkt, options.trim_media))
        .collect();
    ReproRtsp {
        sessions: vec![ReproRtspSession {
            setup_order: (!rtsp.setup_order.is_empty()).then(|| rtsp.setup_order.clone()),
            responses,
            rtp_packets,
        }],
    }
}

fn slice_rtp_packets<'a>(
    packets: &'a [CapturedRtpPacket],
    options: &ExportReproOptions,
) -> Vec<&'a CapturedRtpPacket> {
    let mut selected: Vec<&CapturedRtpPacket> = packets.iter().collect();
    if let Some(range) = options.nalu_range {
        let mut nalu_idx = 0_usize;
        let mut in_selected_nalu = false;
        let mut by_nalu = Vec::new();
        for pkt in selected {
            let starts_new_nalu = is_nalu_start_packet(&pkt.payload);
            if starts_new_nalu {
                in_selected_nalu = nalu_idx >= range.start && nalu_idx < range.end;
                nalu_idx += 1;
            }
            if in_selected_nalu {
                by_nalu.push(pkt);
            }
        }
        selected = by_nalu;
    }
    if let Some(range) = options.packet_range {
        let start = range.start.min(selected.len());
        let end = range.end.min(selected.len());
        selected = selected[start..end].to_vec();
    } else if options.trim_media == TrimMediaMode::HeadersAndMinimalFailingSlice
        && selected.len() > MINIMAL_SLICE_PACKET_CAP
    {
        let start = anomaly_window_start(&selected).unwrap_or(0);
        let end = (start + MINIMAL_SLICE_PACKET_CAP).min(selected.len());
        selected = selected[start..end].to_vec();
    }
    selected
}

fn anomaly_window_start(packets: &[&CapturedRtpPacket]) -> Option<usize> {
    for idx in 1..packets.len() {
        if packets[idx - 1].sequence.wrapping_add(1) != packets[idx].sequence {
            return Some(idx.saturating_sub(4));
        }
    }
    None
}

const fn is_nalu_start_packet(payload: &[u8]) -> bool {
    if payload.is_empty() {
        return true;
    }
    let h264_type = payload[0] & 0x1F;
    if h264_type == 28 && payload.len() >= 2 {
        return (payload[1] & 0x80) != 0;
    }
    let h265_type = (payload[0] >> 1) & 0x3F;
    if h265_type == 49 && payload.len() >= 3 {
        return (payload[2] & 0x80) != 0;
    }
    true
}

fn transform_rtp_packet(pkt: &CapturedRtpPacket, mode: TrimMediaMode) -> ReproRtpPacket {
    let payload_bytes = match mode {
        TrimMediaMode::SyntheticPayloads => synthesize_rtp_payload(&pkt.payload),
        TrimMediaMode::HeadersAndMinimalFailingSlice | TrimMediaMode::KeepRawFrames => {
            pkt.payload.clone()
        }
    };
    ReproRtpPacket {
        channel: pkt.channel,
        track: pkt.track.clone(),
        marker: pkt.marker,
        payload_type: pkt.payload_type,
        sequence: pkt.sequence,
        timestamp: pkt.timestamp,
        ssrc: pkt.ssrc,
        delay_ms: (pkt.delay_ms > 0).then_some(pkt.delay_ms),
        payload_base64: BASE64.encode(payload_bytes),
    }
}

fn synthesize_rtp_payload(payload: &[u8]) -> Vec<u8> {
    if payload.is_empty() {
        return Vec::new();
    }
    let keep_prefix = if (payload[0] & 0x1F) == 28 && payload.len() >= 2 {
        2
    } else if ((payload[0] >> 1) & 0x3F) == 49 && payload.len() >= 3 {
        3
    } else {
        1
    };
    let total_len = payload
        .len()
        .clamp(keep_prefix, SYNTHETIC_PAYLOAD_MAX_BYTES);
    let mut out = vec![0_u8; total_len];
    out[..keep_prefix].copy_from_slice(&payload[..keep_prefix]);
    out
}

fn build_http_exchange(
    ex: &HttpExchangeTranscript,
    scrubber: &TextScrubber<'_>,
) -> ReproHttpExchange {
    let body_text = ex
        .response_body_text
        .as_deref()
        .map(|text| scrubber.scrub_text(text));
    let body_base64 = ex
        .response_body_base64
        .as_deref()
        .and_then(|b64| scrub_base64_payload(b64, scrubber, false));
    let match_body_contains = ex.request_body_text.as_deref().and_then(extract_match_hint);
    ReproHttpExchange {
        match_method: Some(ex.method.clone()),
        match_path_contains: extract_path_fragment(&ex.path),
        match_soap_action: ex.soap_action.clone(),
        match_body_contains,
        status: ex.status,
        status_reason: (!ex.status_reason.is_empty()).then(|| ex.status_reason.clone()),
        headers: scrub_headers(&ex.response_headers, scrubber),
        body_text,
        body_base64,
        delay_ms: (ex.delay_ms > 0).then_some(ex.delay_ms),
    }
}

fn build_reo_frames(
    frames: &[ReoFrameTranscript],
    options: &ExportReproOptions,
    scrubber: &TextScrubber<'_>,
) -> Vec<ReproReoFrame> {
    let server_frames: Vec<&ReoFrameTranscript> = frames
        .iter()
        .filter(|f| f.direction == "server_to_client")
        .collect();
    let sliced = slice_reo_frames(&server_frames, options);
    sliced
        .iter()
        .map(|frame| {
            let payload_xml = frame
                .payload_xml
                .as_deref()
                .map(|xml| scrubber.scrub_text(xml));
            let is_login = frame.msg_id == reo_proto::COMMAND_LOGIN;
            let payload_base64 = match (&frame.payload_base64, options.trim_media) {
                (Some(_), TrimMediaMode::SyntheticPayloads) => Some(BASE64.encode([0_u8; 16])),
                (Some(b64), _) => scrub_base64_payload(b64, scrubber, is_login),
                (None, _) => None,
            };
            ReproReoFrame {
                expected_msg_id: Some(frame.msg_id),
                response_msg_id: frame.msg_id,
                payload_base64,
                payload_xml,
                channel_id: Some(frame.channel_id),
                stream_type: Some(frame.stream_type),
                delay_ms: (frame.delay_ms > 0).then_some(frame.delay_ms),
            }
        })
        .collect()
}

fn slice_reo_frames<'a>(
    server_frames: &'a [&'a ReoFrameTranscript],
    options: &ExportReproOptions,
) -> &'a [&'a ReoFrameTranscript] {
    options.packet_range.map_or_else(
        || {
            if options.trim_media == TrimMediaMode::HeadersAndMinimalFailingSlice
                && server_frames.len() > MINIMAL_SLICE_PACKET_CAP
            {
                &server_frames[..MINIMAL_SLICE_PACKET_CAP]
            } else {
                server_frames
            }
        },
        |range| {
            let start = range.start.min(server_frames.len());
            let end = range.end.min(server_frames.len());
            &server_frames[start..end]
        },
    )
}

fn scrub_base64_payload(
    encoded: &str,
    scrubber: &TextScrubber<'_>,
    strip_entire_login: bool,
) -> Option<String> {
    if !scrubber.strip_credentials && !scrubber.anonymize_network {
        return Some(encoded.to_owned());
    }
    if strip_entire_login && scrubber.strip_credentials {
        return None;
    }
    let Ok(bytes) = BASE64.decode(encoded) else {
        return (!scrubber.strip_credentials).then(|| encoded.to_owned());
    };
    if let Ok(text) = std::str::from_utf8(&bytes) {
        return Some(BASE64.encode(scrubber.scrub_text(text).as_bytes()));
    }
    if scrubber.strip_credentials && contains_binary_secret(&bytes, scrubber.extra_secrets) {
        return None;
    }
    Some(encoded.to_owned())
}

fn contains_binary_secret(bytes: &[u8], extra_secrets: &[String]) -> bool {
    if extra_secrets.iter().any(|secret| {
        !secret.is_empty() && bytes.windows(secret.len()).any(|w| w == secret.as_bytes())
    }) {
        return true;
    }
    let lossy = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    lossy.contains("authorization: basic ")
        || lossy.contains("authorization: digest ")
        || lossy.contains("\"password\"")
        || lossy.contains("token=")
}

fn build_safety_report(
    options: &ExportReproOptions,
    mut warnings: Vec<String>,
) -> SharingSafetyReport {
    if !options.strip_credentials {
        warnings.push(
            "credentials were not stripped; Authorization headers, tokens, or passwords may be exposed"
                .to_owned(),
        );
    }
    if !options.anonymize_network {
        warnings.push(
            "network identifiers were not anonymized; local IP addresses or hostnames may be exposed"
                .to_owned(),
        );
    }
    if options.trim_media == TrimMediaMode::KeepRawFrames {
        warnings.push(
            "raw media frames retained; fixture may contain identifiable camera video or audio"
                .to_owned(),
        );
    }
    let sanitized = options.strip_credentials && options.anonymize_network;
    let safe_to_share_publicly =
        sanitized && options.trim_media != TrimMediaMode::KeepRawFrames && warnings.is_empty();
    SharingSafetyReport {
        safe_to_share_publicly,
        sanitized,
        strip_credentials: options.strip_credentials,
        anonymize_network: options.anonymize_network,
        trim_media: options.trim_media,
        warnings,
    }
}

fn audit_exported_fixture(
    fixture: &CameraReproFixture,
    options: &ExportReproOptions,
) -> anyhow::Result<Vec<String>> {
    let serialized = serde_json::to_string(fixture)?;
    ensure!(
        serialized.len() <= MAX_REPRO_FIXTURE_BYTES,
        "exported fixture size {} exceeds {MAX_REPRO_FIXTURE_BYTES}",
        serialized.len()
    );
    let mut warnings = Vec::new();
    audit_text_payload(&serialized, options, &mut warnings);
    for b64 in collect_fixture_base64_blobs(fixture) {
        if let Ok(decoded) = BASE64.decode(b64) {
            let lossy = String::from_utf8_lossy(&decoded);
            audit_text_payload(&lossy, options, &mut warnings);
        }
    }
    warnings.sort();
    warnings.dedup();
    Ok(warnings)
}

fn collect_fixture_base64_blobs(fixture: &CameraReproFixture) -> Vec<&str> {
    let mut blobs = Vec::new();
    if let Some(http) = &fixture.http {
        blobs.extend(
            http.exchanges
                .iter()
                .filter_map(|ex| ex.body_base64.as_deref()),
        );
    }
    if let Some(reo) = &fixture.reo_proto {
        blobs.extend(
            reo.frames
                .iter()
                .filter_map(|frame| frame.payload_base64.as_deref()),
        );
    }
    blobs
}

fn audit_text_payload(text: &str, options: &ExportReproOptions, warnings: &mut Vec<String>) {
    if options.strip_credentials {
        if options
            .extra_secrets
            .iter()
            .any(|secret| !secret.is_empty() && text.contains(secret))
        {
            warnings.push("residual secret matched caller extra_secrets".to_owned());
        }
        let lower = text.to_ascii_lowercase();
        if lower.contains("authorization: basic ")
            || lower.contains("authorization: digest ")
            || lower.contains("authorization\",\"basic ")
            || lower.contains("authorization\",\"digest ")
        {
            warnings
                .push("residual Authorization header found after credential scrubbing".to_owned());
        }
    }
    if options.anonymize_network && has_non_testnet_ipv4(text) {
        warnings.push("non-TEST-NET IPv4 literal remained after anonymization".to_owned());
    }
}

pub(crate) fn scrub_wire_text(input: &str, extra_secrets: &[String]) -> String {
    let scrubber = TextScrubber {
        strip_credentials: true,
        anonymize_network: false,
        source_host: "",
        extra_secrets,
    };
    scrubber.scrub_text(input)
}

pub(crate) fn is_sensitive_header_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization"
            | "proxy-authorization"
            | "cookie"
            | "set-cookie"
            | "x-auth-token"
            | "x-api-key"
    )
}

struct TextScrubber<'a> {
    strip_credentials: bool,
    anonymize_network: bool,
    source_host: &'a str,
    extra_secrets: &'a [String],
}

impl TextScrubber<'_> {
    fn scrub_text(&self, input: &str) -> String {
        let mut out = input.to_owned();
        if self.strip_credentials {
            for secret in self.extra_secrets {
                if !secret.is_empty() {
                    out = out.replace(secret, "[REDACTED]");
                }
            }
            for tag in [
                "Password", "password", "Username", "username", "userName", "UserName", "Nonce",
                "nonce",
            ] {
                out = redact_xml_tag(&out, tag);
            }
            for key in ["password", "userName", "token", "name"] {
                out = redact_json_string_field(&out, key);
            }
            out = redact_query_param(&out, "token");
            out = redact_userinfo(&out);
        }
        if self.anonymize_network {
            if !self.source_host.is_empty() {
                out = out.replace(self.source_host, ANONYMIZED_CAMERA_IP);
            }
            out = anonymize_ipv4_literals(&out);
        }
        out
    }
}

fn scrub_headers(
    headers: &[(String, String)],
    scrubber: &TextScrubber<'_>,
) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(k, v)| {
            let val = if scrubber.strip_credentials && is_sensitive_header_name(k) {
                "[REDACTED]".to_owned()
            } else {
                scrubber.scrub_text(v)
            };
            (k.clone(), val)
        })
        .collect()
}

fn redact_xml_tag(input: &str, tag: &str) -> String {
    let mut out = input.to_owned();
    let close_needle = "</";
    let mut search_from = 0;
    while let Some(open_pos) = out[search_from..].find(tag) {
        let abs_tag = search_from + open_pos;
        let after_tag = abs_tag + tag.len();
        let next_byte = out.as_bytes().get(after_tag).copied();
        let valid_tag_boundary = abs_tag > 0
            && matches!(out.as_bytes()[abs_tag - 1], b'<' | b':')
            && next_byte.is_some_and(|b| b == b'>' || b.is_ascii_whitespace());
        if !valid_tag_boundary {
            search_from = after_tag;
            continue;
        }
        let Some(open_end_rel) = out[after_tag..].find('>') else {
            break;
        };
        if out[after_tag..after_tag + open_end_rel].ends_with('/') {
            search_from = after_tag + open_end_rel + 1;
            continue;
        }
        let val_start = after_tag + open_end_rel + 1;
        let Some(rel_close) = out[val_start..].find(close_needle) else {
            break;
        };
        let val_end = val_start + rel_close;
        out.replace_range(val_start..val_end, "[REDACTED]");
        search_from = val_start + "[REDACTED]".len();
    }
    out
}

fn redact_json_string_field(input: &str, field: &str) -> String {
    let pattern = format!("\"{field}\":");
    let mut out = input.to_owned();
    let mut pos = 0;
    while let Some(found) = out[pos..].find(&pattern) {
        let after_colon = pos + found + pattern.len();
        let trimmed = out[after_colon..].trim_start();
        let ws_len = out[after_colon..].len() - trimmed.len();
        let quote_pos = after_colon + ws_len;
        if out.as_bytes().get(quote_pos) == Some(&b'"')
            && let Some(end_quote) = out[quote_pos + 1..].find('"')
        {
            let val_start = quote_pos + 1;
            let val_end = val_start + end_quote;
            out.replace_range(val_start..val_end, "[REDACTED]");
            pos = val_start + "[REDACTED]".len();
            continue;
        }
        pos = after_colon;
    }
    out
}

fn redact_query_param(input: &str, key: &str) -> String {
    let needle = format!("{key}=");
    let mut out = input.to_owned();
    let mut pos = 0;
    while let Some(found) = out[pos..].find(&needle) {
        let start = pos + found + needle.len();
        let end = out[start..]
            .find(['&', ' ', '\r', '\n', '"', '\''])
            .map_or(out.len(), |rel| start + rel);
        out.replace_range(start..end, "REDACTED");
        pos = start + "REDACTED".len();
    }
    out
}

fn redact_userinfo(input: &str) -> String {
    let mut out = input.to_owned();
    for scheme in ["rtsp://", "rtsps://", "http://", "https://"] {
        let mut pos = 0;
        while let Some(found) = out[pos..].find(scheme) {
            let auth_start = pos + found + scheme.len();
            let boundary = out[auth_start..]
                .find(['/', ' ', '\r', '\n', '"', '<'])
                .map_or(out.len(), |rel| auth_start + rel);
            if let Some(at_rel) = out[auth_start..boundary].find('@') {
                out.replace_range(auth_start..auth_start + at_rel, "redacted:redacted");
                pos = auth_start + "redacted:redacted@".len();
            } else {
                pos = boundary;
            }
        }
    }
    out
}

fn anonymize_ipv4_literals(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit()
            && let Some((end, ip)) = parse_ipv4_at(input, i)
            && should_anonymize_ipv4(ip)
        {
            out.push_str(ANONYMIZED_CAMERA_IP);
            i = end;
            continue;
        }
        let ch = input[i..].chars().next().expect("valid UTF-8 char");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn has_non_testnet_ipv4(input: &str) -> bool {
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit()
            && let Some((end, ip)) = parse_ipv4_at(input, i)
        {
            if should_anonymize_ipv4(ip) {
                return true;
            }
            i = end;
            continue;
        }
        i += 1;
    }
    false
}

fn parse_ipv4_at(input: &str, start: usize) -> Option<(usize, Ipv4Addr)> {
    if start > 0 && input.as_bytes()[start - 1].is_ascii_digit() {
        return None;
    }
    let mut end = start;
    while end < input.len()
        && (input.as_bytes()[end].is_ascii_digit() || input.as_bytes()[end] == b'.')
    {
        end += 1;
    }
    let candidate = &input[start..end];
    if candidate.matches('.').count() != 3 {
        return None;
    }
    let ip: Ipv4Addr = candidate.parse().ok()?;
    Some((end, ip))
}

const fn should_anonymize_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    let is_testnet = (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
        || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
        || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113);
    !is_testnet && !ip.is_unspecified()
}

fn extract_path_fragment(uri_or_path: &str) -> Option<String> {
    if let Ok(parsed) = url::Url::parse(uri_or_path) {
        let p = parsed.path();
        return (!p.is_empty()).then(|| p.to_owned());
    }
    let path = uri_or_path.split('?').next().unwrap_or(uri_or_path);
    (!path.is_empty()).then(|| path.to_owned())
}

fn extract_match_hint(body: &str) -> Option<String> {
    for marker in [
        "GetServices",
        "GetDeviceInformation",
        "GetProfiles",
        "GetStreamUri",
    ] {
        if body.contains(marker) {
            return Some(marker.to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera_debug::protocols::{RtspExchangeTranscript, TRANSCRIPT_SCHEMA_VERSION};

    fn sample_rtsp_transcript() -> ProtocolTranscript {
        ProtocolTranscript {
            schema_version: TRANSCRIPT_SCHEMA_VERSION.to_owned(),
            camera_id: "front-gate".to_owned(),
            camera_host: "192.168.1.55".to_owned(),
            protocol: CaptureProtocol::Rtsp,
            tls_decrypted: false,
            include_raw_auth: true,
            rtsp: Some(RtspTranscript {
                sdp: Some("v=0\r\no=- 0 0 IN IP4 192.168.1.55\r\n".to_owned()),
                setup_order: vec!["video".to_owned()],
                exchanges: vec![RtspExchangeTranscript {
                    method: "DESCRIBE".to_owned(),
                    uri: "rtsp://admin:hunter2@192.168.1.55:554/h264Preview_01_main".to_owned(),
                    request_headers: vec![("Authorization".to_owned(), "Basic YWRtaW46aHVudGVyMg==".to_owned())],
                    request_body: String::new(),
                    status: 200,
                    reason: "OK".to_owned(),
                    response_headers: vec![("Content-Base".to_owned(), "rtsp://192.168.1.55:554/".to_owned())],
                    response_body: "v=0\r\no=- 0 0 IN IP4 192.168.1.55\r\na=control:rtsp://192.168.1.55/trackID=0".to_owned(),
                    delay_ms: 10,
                }],
                rtp_packets: vec![
                    CapturedRtpPacket {
                        track: "video".to_owned(),
                        channel: Some(0),
                        marker: false,
                        payload_type: 96,
                        sequence: 100,
                        timestamp: 90_000,
                        ssrc: 0x1122_3344,
                        delay_ms: 15,
                        payload: vec![0x7C, 0x85, 0x11, 0x22, 0x33],
                    },
                    CapturedRtpPacket {
                        track: "video".to_owned(),
                        channel: Some(0),
                        marker: true,
                        payload_type: 96,
                        sequence: 101,
                        timestamp: 90_000,
                        ssrc: 0x1122_3344,
                        delay_ms: 20,
                        payload: vec![0x7C, 0x45, 0x44, 0x55],
                    },
                    CapturedRtpPacket {
                        track: "video".to_owned(),
                        channel: Some(0),
                        marker: true,
                        payload_type: 96,
                        sequence: 103,
                        timestamp: 93_000,
                        ssrc: 0x1122_3344,
                        delay_ms: 50,
                        payload: vec![0x41, 0xAA, 0xBB],
                    },
                ],
            }),
            http: Vec::new(),
            reo_proto: Vec::new(),
        }
    }

    #[test]
    fn exports_sanitized_and_anonymized_fixture_for_public_sharing() {
        let transcript = sample_rtsp_transcript();
        let options = ExportReproOptions {
            extra_secrets: vec!["hunter2".to_owned()],
            ..ExportReproOptions::default()
        };
        let fixture = export_repro_fixture_from_transcript(&transcript, &options).unwrap();
        assert_eq!(fixture.schema_version, REPRO_SCHEMA_VERSION);
        assert!(fixture.camera.anonymized);
        assert!(fixture.camera.credentials_stripped);
        assert_eq!(fixture.camera.base_host, ANONYMIZED_HOST);
        assert!(fixture.sharing_safety_report.safe_to_share_publicly);
        assert!(fixture.sharing_safety_report.warnings.is_empty());

        let json = serde_json::to_string(&fixture).unwrap();
        assert!(!json.contains("192.168.1.55"));
        assert!(!json.contains("hunter2"));
        assert!(json.contains(ANONYMIZED_CAMERA_IP));
    }

    #[test]
    fn flags_unsafe_options_and_supports_synthetic_and_nalu_slicing() {
        let transcript = sample_rtsp_transcript();
        let mut raw_opts = ExportReproOptions::default();
        raw_opts.strip_credentials = false;
        raw_opts.anonymize_network = false;
        raw_opts.trim_media = TrimMediaMode::KeepRawFrames;
        let unsafe_fixture = export_repro_fixture_from_transcript(&transcript, &raw_opts).unwrap();
        assert!(!unsafe_fixture.sharing_safety_report.safe_to_share_publicly);
        assert_eq!(unsafe_fixture.sharing_safety_report.warnings.len(), 3);

        let mut synth_opts = ExportReproOptions::default();
        synth_opts.trim_media = TrimMediaMode::SyntheticPayloads;
        synth_opts.nalu_range = Some(IndexRange { start: 0, end: 1 });
        let synth_fixture = export_repro_fixture_from_transcript(&transcript, &synth_opts).unwrap();
        let pkts = &synth_fixture.rtsp.as_ref().unwrap().sessions[0].rtp_packets;
        assert_eq!(pkts.len(), 2);
        let first_payload = BASE64.decode(&pkts[0].payload_base64).unwrap();
        assert_eq!(first_payload, vec![0x7C, 0x85, 0x00, 0x00, 0x00]);
        assert!(synth_fixture.sharing_safety_report.safe_to_share_publicly);
    }

    #[test]
    fn scrubs_http_and_reo_proto_credentials_and_ips() {
        let transcript = ProtocolTranscript {
            schema_version: TRANSCRIPT_SCHEMA_VERSION.to_owned(),
            camera_id: "cam-2".to_owned(),
            camera_host: "10.0.0.42".to_owned(),
            protocol: CaptureProtocol::ReolinkHttp,
            tls_decrypted: true,
            include_raw_auth: true,
            rtsp: None,
            http: vec![HttpExchangeTranscript {
                method: "POST".to_owned(),
                path: "/cgi-bin/api.cgi?cmd=Login&token=secret-tok".to_owned(),
                soap_action: None,
                request_headers: vec![(
                    "Authorization".to_owned(),
                    "Basic dXNlcjpwYXNz".to_owned(),
                )],
                request_body_text: Some(
                    r#"[{"cmd":"Login","param":{"User":{"userName":"admin","password":"pw123"}}}]"#
                        .to_owned(),
                ),
                status: 200,
                status_reason: "OK".to_owned(),
                response_headers: vec![("Set-Cookie".to_owned(), "sid=abc".to_owned())],
                response_body_text: Some(
                    r#"[{"cmd":"Login","value":{"Token":{"name":"secret-tok"}}}]"#.to_owned(),
                ),
                response_body_base64: None,
                delay_ms: 5,
            }],
            reo_proto: vec![ReoFrameTranscript {
                direction: "server_to_client".to_owned(),
                msg_id: 1,
                channel_id: 0,
                stream_type: 0,
                payload_xml: Some(
                    "<Login><password>pw123</password><ip>10.0.0.42</ip></Login>".to_owned(),
                ),
                payload_base64: None,
                delay_ms: 12,
            }],
        };
        let fixture =
            export_repro_fixture_from_transcript(&transcript, &ExportReproOptions::default())
                .unwrap();
        let json = serde_json::to_string(&fixture).unwrap();
        assert!(!json.contains("secret-tok"));
        assert!(!json.contains("pw123"));
        assert!(!json.contains("10.0.0.42"));
        assert!(fixture.sharing_safety_report.safe_to_share_publicly);
    }

    fn sample_sensitive_transcript() -> ProtocolTranscript {
        ProtocolTranscript {
            schema_version: TRANSCRIPT_SCHEMA_VERSION.to_owned(),
            camera_id: "cam-xml".to_owned(),
            camera_host: "10.0.0.42".to_owned(),
            protocol: CaptureProtocol::Onvif,
            tls_decrypted: false,
            include_raw_auth: true,
            rtsp: None,
            http: vec![HttpExchangeTranscript {
                method: "POST".to_owned(),
                path: "/onvif/device_service".to_owned(),
                soap_action: None,
                request_headers: vec![
                    (
                        "Proxy-Authorization".to_owned(),
                        "Basic YWRtaW46cHc=".to_owned(),
                    ),
                    ("X-Api-Key".to_owned(), "api-key-999".to_owned()),
                ],
                request_body_text: Some(
                    r#"<wsse:Username>camuser</wsse:Username><wsse:Password Type="http://example#Digest">digest-pw</wsse:Password><wsse:Nonce EncodingType="http://example#Base64">nonce-val</wsse:Nonce>"#
                        .to_owned(),
                ),
                status: 200,
                status_reason: "OK".to_owned(),
                response_headers: vec![("X-Auth-Token".to_owned(), "tok-hdr".to_owned())],
                response_body_text: None,
                response_body_base64: Some(BASE64.encode(b"payload-secret-xyz")),
                delay_ms: 4,
            }],
            reo_proto: vec![ReoFrameTranscript {
                direction: "client_to_server".to_owned(),
                msg_id: 1,
                channel_id: 0,
                stream_type: 0,
                payload_xml: Some(
                    "<Username>camuser</Username><Password>pw123</Password>".to_owned(),
                ),
                payload_base64: Some(BASE64.encode(b"raw-login-bytes")),
                delay_ms: 8,
            }],
        }
    }

    #[test]
    fn scrubs_xml_attributes_headers_and_base64_and_flags_base64_leak() {
        let transcript = sample_sensitive_transcript();
        let mut safe_opts = ExportReproOptions::default();
        safe_opts.extra_secrets = vec!["payload-secret-xyz".to_owned()];
        let safe = export_repro_fixture_from_transcript(&transcript, &safe_opts).unwrap();
        let safe_json = serde_json::to_string(&safe).unwrap();
        for leaked in [
            "digest-pw",
            "nonce-val",
            "camuser",
            "pw123",
            "payload-secret-xyz",
            "raw-login-bytes",
            "api-key-999",
            "tok-hdr",
        ] {
            assert!(!safe_json.contains(leaked), "leaked {leaked}");
        }
        assert!(safe.sharing_safety_report.safe_to_share_publicly);

        let mut raw_opts = ExportReproOptions::default();
        raw_opts.strip_credentials = false;
        raw_opts.extra_secrets = vec!["payload-secret-xyz".to_owned()];
        let leaked = export_repro_fixture_from_transcript(&transcript, &raw_opts).unwrap();
        assert!(!leaked.sharing_safety_report.safe_to_share_publicly);
        assert!(!leaked.sharing_safety_report.sanitized);
        assert!(!leaked.sharing_safety_report.warnings.is_empty());

        let mut ip_fixture = safe;
        ip_fixture.http.as_mut().unwrap().exchanges[0].body_base64 =
            Some(BASE64.encode(b"peer=10.20.30.40"));
        let audit = audit_exported_fixture(&ip_fixture, &ExportReproOptions::default()).unwrap();
        assert!(audit.iter().any(|w| w.contains("IPv4")));
    }
}
