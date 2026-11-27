//! Elementary video/audio bitstream writers, MP4 side-by-side muxer, and anomaly detector.
//!
//! Writes raw Annex-B `.h264`/`.h265`, raw `.aac` (ADTS), `.pcmu`/`.pcma`, or
//! `.opus` elementary streams, optional raw RTP packet dumps, and a KeepPeek
//! `muxed.mp4` file while counting SPS/PPS/VPS, IDR, FU-A, RTP sequence,
//! DTS/PTS regression, and A/V drift anomalies.

use crate::storage::{adts::parse_adts, nal::annexb_to_avcc};
use anyhow::{Context, ensure};
use bytes::Bytes;
use mp4::{
    AacConfig, AudioObjectType, AvcConfig, ChannelConfig, HevcConfig, MediaConfig, Mp4Config,
    Mp4Sample, Mp4Writer, SampleFreqIndex, TrackConfig, TrackType,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

const ANNEXB_START_CODE: [u8; 4] = [0, 0, 0, 1];
const MAX_PENDING_MP4_SAMPLES: usize = 512;
const MAX_PENDING_MP4_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_MP4_TIMESCALE: u32 = 90_000;
const DEFAULT_FRAME_DURATION_TICKS: u32 = 3_000;
const RTP_MIN_HEADER_LEN: usize = 12;

/// Video bitstream codec supported by [`BitstreamRecorder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoCodecKind {
    H264,
    H265,
}

impl VideoCodecKind {
    /// Returns the elementary stream file extension without a leading dot.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::H265 => "h265",
        }
    }
}

/// Audio elementary codec supported by [`BitstreamRecorder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioElementaryCodec {
    Aac,
    Pcmu,
    Pcma,
    Opus,
}

impl AudioElementaryCodec {
    /// Returns the elementary audio file extension without a leading dot.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Aac => "aac",
            Self::Pcmu => "pcmu",
            Self::Pcma => "pcma",
            Self::Opus => "opus",
        }
    }
}

/// Video framing layout supplied by a caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoFraming {
    AnnexB,
    FourByteAvcc,
}

/// Audio configuration used when synthesizing ADTS headers and MP4 AAC sample entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioStreamParams {
    pub codec: AudioElementaryCodec,
    pub sample_rate_hz: u32,
    pub channels: u8,
    pub aac_profile: u8,
}

/// Video dimensions and optional parameter sets known ahead of stream frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoStreamParams {
    pub codec: VideoCodecKind,
    pub width: u16,
    pub height: u16,
    pub sps: Option<Vec<u8>>,
    pub pps: Option<Vec<u8>>,
    pub vps: Option<Vec<u8>>,
}

/// Parsed raw RTP packet retained for repro export and anomaly auditing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedRtpPacket {
    pub track: String,
    pub channel: Option<u8>,
    pub marker: bool,
    pub payload_type: u8,
    pub sequence: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    pub delay_ms: u64,
    pub payload: Vec<u8>,
}

/// Automated video, audio, and RTP anomaly summary for one capture.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BitstreamAnomalyReport {
    pub video_codec: Option<VideoCodecKind>,
    pub audio_codec: Option<AudioElementaryCodec>,
    pub video_frames: u64,
    pub audio_frames: u64,
    pub rtp_packets: u64,
    pub malformed_rtp_packets: u64,
    pub sps_count: u64,
    pub pps_count: u64,
    pub vps_count: u64,
    pub has_sps: bool,
    pub has_pps: bool,
    pub has_vps: bool,
    pub idr_count: u64,
    pub missing_idr: bool,
    pub fua_start_errors: u64,
    pub fua_end_errors: u64,
    pub rtp_seq_gaps: u64,
    pub dts_pts_regressions: u64,
    pub av_drift_ms_latest: Option<i64>,
    pub av_drift_ms_max: Option<u64>,
}

/// Paths of media artifacts produced inside one capture directory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BitstreamOutputs {
    pub video_path: Option<PathBuf>,
    pub audio_path: Option<PathBuf>,
    pub muxed_mp4_path: Option<PathBuf>,
    pub rtp_dump_path: Option<PathBuf>,
    pub anomalies: BitstreamAnomalyReport,
    pub bytes_written: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct FuaTrackState {
    in_fragment: bool,
    last_seq: Option<u16>,
    last_timestamp: Option<u32>,
}

#[derive(Debug, Clone)]
struct PendingSample {
    is_audio: bool,
    pts_90k: u64,
    duration_90k: u32,
    is_sync: bool,
    bytes: Bytes,
}

/// Bounded writer for elementary bitstreams, raw RTP dumps, `muxed.mp4`, and anomaly counts.
pub struct BitstreamRecorder {
    capture_dir: PathBuf,
    max_bytes: u64,
    bytes_written: u64,
    write_rtp_dump: bool,
    video_params: Option<VideoStreamParams>,
    audio_params: Option<AudioStreamParams>,
    video_file: Option<BufWriter<File>>,
    video_path: Option<PathBuf>,
    audio_file: Option<BufWriter<File>>,
    audio_path: Option<PathBuf>,
    rtp_dump_file: Option<BufWriter<File>>,
    rtp_dump_path: Option<PathBuf>,
    mp4_writer: Option<Mp4Writer<BufWriter<File>>>,
    mp4_path: Option<PathBuf>,
    mp4_failed: bool,
    mp4_video_track_id: Option<u32>,
    mp4_audio_track_id: Option<u32>,
    pending_mp4: Vec<PendingSample>,
    pending_mp4_bytes: usize,
    last_video_pts_us: Option<u64>,
    last_audio_pts_us: Option<u64>,
    first_video_pts_us: Option<u64>,
    first_audio_pts_us: Option<u64>,
    fua_tracks: HashMap<String, FuaTrackState>,
    rtp_first_ts_us: Option<u64>,
    rtp_packets: Vec<CapturedRtpPacket>,
    report: BitstreamAnomalyReport,
}

impl BitstreamRecorder {
    /// Creates a recorder writing bounded elementary streams and `muxed.mp4` into `capture_dir`.
    ///
    /// # Errors
    /// Returns an error if `max_bytes` is zero or `capture_dir` cannot be created.
    pub fn new(
        capture_dir: impl AsRef<Path>,
        max_bytes: u64,
        write_rtp_dump: bool,
    ) -> anyhow::Result<Self> {
        ensure!(max_bytes > 0, "bitstream max_bytes must be positive");
        let capture_dir = capture_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&capture_dir)?;
        Ok(Self {
            capture_dir,
            max_bytes,
            bytes_written: 0,
            write_rtp_dump,
            video_params: None,
            audio_params: None,
            video_file: None,
            video_path: None,
            audio_file: None,
            audio_path: None,
            rtp_dump_file: None,
            rtp_dump_path: None,
            mp4_writer: None,
            mp4_path: None,
            mp4_failed: false,
            mp4_video_track_id: None,
            mp4_audio_track_id: None,
            pending_mp4: Vec::new(),
            pending_mp4_bytes: 0,
            last_video_pts_us: None,
            last_audio_pts_us: None,
            first_video_pts_us: None,
            first_audio_pts_us: None,
            fua_tracks: HashMap::new(),
            rtp_first_ts_us: None,
            rtp_packets: Vec::new(),
            report: BitstreamAnomalyReport::default(),
        })
    }

    /// Configures video codec metadata before or during capture.
    pub fn configure_video(&mut self, params: VideoStreamParams) {
        self.report.video_codec = Some(params.codec);
        if let Some(sps) = &params.sps {
            self.record_parameter_nalu(params.codec, sps);
        }
        if let Some(pps) = &params.pps {
            self.record_parameter_nalu(params.codec, pps);
        }
        if let Some(vps) = &params.vps {
            self.record_parameter_nalu(params.codec, vps);
        }
        self.video_params = Some(params);
    }

    /// Configures audio stream metadata before or during capture.
    pub const fn configure_audio(&mut self, params: AudioStreamParams) {
        self.report.audio_codec = Some(params.codec);
        self.audio_params = Some(params);
    }

    /// Records one raw RTP packet, updating sequence, DTS/PTS, and FU-A/FU anomaly counters.
    ///
    /// # Errors
    /// Returns an error if writing the optional raw RTP dump file fails.
    pub fn record_rtp_packet(
        &mut self,
        track: &str,
        channel: Option<u8>,
        video_codec_hint: Option<VideoCodecKind>,
        timestamp_us: u64,
        raw_rtp: &[u8],
    ) -> anyhow::Result<()> {
        self.report.rtp_packets += 1;
        let Ok(parsed) = parse_rtp_packet(raw_rtp) else {
            self.report.malformed_rtp_packets += 1;
            return Ok(());
        };
        let state = self.fua_tracks.entry(track.to_owned()).or_default();
        let seq_gap = check_rtp_sequence(state, parsed.sequence);
        if seq_gap {
            self.report.rtp_seq_gaps += 1;
            if state.in_fragment {
                self.report.fua_end_errors += 1;
                state.in_fragment = false;
            }
        }
        if let Some(prev_ts) = state.last_timestamp
            && (parsed.timestamp.wrapping_sub(prev_ts) as i32) < 0
        {
            self.report.dts_pts_regressions += 1;
        }
        state.last_timestamp = Some(parsed.timestamp);
        if let Some(codec) = video_codec_hint {
            let (start_err, end_err) = update_fu_state(state, codec, parsed.payload, parsed.marker);
            self.report.fua_start_errors += u64::from(start_err);
            self.report.fua_end_errors += u64::from(end_err);
        }
        let origin_us = *self.rtp_first_ts_us.get_or_insert(timestamp_us);
        let delay_ms = timestamp_us.saturating_sub(origin_us) / 1_000;
        if self.bytes_written.saturating_add(raw_rtp.len() as u64) <= self.max_bytes {
            self.rtp_packets.push(CapturedRtpPacket {
                track: track.to_owned(),
                channel,
                marker: parsed.marker,
                payload_type: parsed.payload_type,
                sequence: parsed.sequence,
                timestamp: parsed.timestamp,
                ssrc: parsed.ssrc,
                delay_ms,
                payload: parsed.payload.to_vec(),
            });
            if self.write_rtp_dump {
                self.append_rtp_dump(track, channel, timestamp_us, raw_rtp)?;
            }
        }
        Ok(())
    }

    /// Records one depacketized video frame, writing Annex-B elementary bytes and `muxed.mp4`.
    ///
    /// # Errors
    /// Returns an error if file creation or elementary stream writing fails.
    pub fn record_video_frame(
        &mut self,
        codec: VideoCodecKind,
        framing: VideoFraming,
        pts_us: u64,
        is_sync_hint: bool,
        frame_bytes: &[u8],
    ) -> anyhow::Result<()> {
        self.report.video_codec.get_or_insert(codec);
        self.report.video_frames += 1;
        self.update_video_timing(pts_us);

        let avcc = match framing {
            VideoFraming::AnnexB => annexb_to_avcc(frame_bytes),
            VideoFraming::FourByteAvcc => frame_bytes.to_vec(),
        };
        let mut nalus = Vec::new();
        collect_avcc_nalus(&avcc, &mut nalus);
        let mut has_idr = is_sync_hint;
        for nalu in &nalus {
            has_idr |= self.inspect_video_nalu(codec, nalu);
        }
        if has_idr {
            self.report.idr_count += 1;
        }
        self.write_video_annexb(codec, &nalus)?;
        let mp4_payload = filter_mp4_video_nalus(codec, &nalus);
        if !mp4_payload.is_empty() {
            let pts_90k = pts_us.saturating_mul(90) / 1_000;
            self.push_mp4_sample(PendingSample {
                is_audio: false,
                pts_90k,
                duration_90k: DEFAULT_FRAME_DURATION_TICKS,
                is_sync: has_idr,
                bytes: Bytes::from(mp4_payload),
            })?;
        }
        Ok(())
    }

    /// Records one audio frame, writing raw `.aac` (ADTS), `.pcmu`, `.pcma`, or `.opus`.
    ///
    /// # Errors
    /// Returns an error if elementary audio writing fails.
    pub fn record_audio_frame(
        &mut self,
        params: AudioStreamParams,
        pts_us: u64,
        frame_bytes: &[u8],
    ) -> anyhow::Result<()> {
        self.configure_audio(params);
        self.report.audio_frames += 1;
        self.update_audio_timing(pts_us);
        let elem = match params.codec {
            AudioElementaryCodec::Aac => ensure_adts_frame(params, frame_bytes),
            AudioElementaryCodec::Pcmu
            | AudioElementaryCodec::Pcma
            | AudioElementaryCodec::Opus => frame_bytes.to_vec(),
        };
        self.write_audio_elementary(params.codec, &elem)?;
        if params.codec == AudioElementaryCodec::Aac {
            let raw =
                parse_adts(frame_bytes).map_or(frame_bytes, |info| &frame_bytes[info.header_len..]);
            if !raw.is_empty() {
                let pts_90k = pts_us.saturating_mul(90) / 1_000;
                self.push_mp4_sample(PendingSample {
                    is_audio: true,
                    pts_90k,
                    duration_90k: audio_duration_90k(params.sample_rate_hz),
                    is_sync: true,
                    bytes: Bytes::copy_from_slice(raw),
                })?;
            }
        }
        Ok(())
    }

    /// Returns a snapshot of the anomaly report computed so far.
    #[must_use]
    pub fn anomaly_report(&self) -> BitstreamAnomalyReport {
        let mut report = self.report.clone();
        report.missing_idr = report.video_frames > 0 && report.idr_count == 0;
        for state in self.fua_tracks.values() {
            if state.in_fragment {
                report.fua_end_errors += 1;
            }
        }
        report
    }

    /// Returns captured raw RTP packets buffered for repro fixture export.
    #[must_use]
    pub fn captured_rtp_packets(&self) -> &[CapturedRtpPacket] {
        &self.rtp_packets
    }

    /// Flushes all open elementary files and finalizes `muxed.mp4`.
    ///
    /// # Errors
    /// Returns an error if flushing files or finalizing the MP4 header fails.
    pub fn finish(mut self) -> anyhow::Result<BitstreamOutputs> {
        let report = self.anomaly_report();
        if let Some(mut file) = self.video_file.take() {
            file.flush()?;
        }
        if let Some(mut file) = self.audio_file.take() {
            file.flush()?;
        }
        if let Some(mut file) = self.rtp_dump_file.take() {
            file.flush()?;
        }
        self.try_init_mp4()?;
        if let Some(mut mp4) = self.mp4_writer.take() {
            mp4.write_end().context("finalize muxed.mp4")?;
        }
        Ok(BitstreamOutputs {
            video_path: self.video_path,
            audio_path: self.audio_path,
            muxed_mp4_path: self.mp4_path,
            rtp_dump_path: self.rtp_dump_path,
            anomalies: report,
            bytes_written: self.bytes_written,
        })
    }

    fn update_video_timing(&mut self, pts_us: u64) {
        self.first_video_pts_us.get_or_insert(pts_us);
        if let Some(prev) = self.last_video_pts_us
            && pts_us < prev
        {
            self.report.dts_pts_regressions += 1;
        }
        self.last_video_pts_us = Some(pts_us);
        self.update_av_drift();
    }

    fn update_audio_timing(&mut self, pts_us: u64) {
        self.first_audio_pts_us.get_or_insert(pts_us);
        if let Some(prev) = self.last_audio_pts_us
            && pts_us < prev
        {
            self.report.dts_pts_regressions += 1;
        }
        self.last_audio_pts_us = Some(pts_us);
        self.update_av_drift();
    }

    fn update_av_drift(&mut self) {
        let (Some(v_first), Some(v_last), Some(a_first), Some(a_last)) = (
            self.first_video_pts_us,
            self.last_video_pts_us,
            self.first_audio_pts_us,
            self.last_audio_pts_us,
        ) else {
            return;
        };
        let v_rel = i128::from(v_last) - i128::from(v_first);
        let a_rel = i128::from(a_last) - i128::from(a_first);
        let drift_ms = i64::try_from((a_rel - v_rel) / 1_000).unwrap_or(i64::MAX);
        self.report.av_drift_ms_latest = Some(drift_ms);
        let abs_drift = drift_ms.unsigned_abs();
        self.report.av_drift_ms_max = Some(
            self.report
                .av_drift_ms_max
                .map_or(abs_drift, |prev| prev.max(abs_drift)),
        );
    }

    fn inspect_video_nalu(&mut self, codec: VideoCodecKind, nalu: &[u8]) -> bool {
        if nalu.is_empty() {
            return false;
        }
        self.record_parameter_nalu(codec, nalu);
        match codec {
            VideoCodecKind::H264 => (nalu[0] & 0x1F) == 5,
            VideoCodecKind::H265 => matches!((nalu[0] >> 1) & 0x3F, 19..=21),
        }
    }

    fn record_parameter_nalu(&mut self, codec: VideoCodecKind, nalu: &[u8]) {
        let params = self.video_params.get_or_insert(VideoStreamParams {
            codec,
            width: 1280,
            height: 720,
            sps: None,
            pps: None,
            vps: None,
        });
        match codec {
            VideoCodecKind::H264 if !nalu.is_empty() => match nalu[0] & 0x1F {
                7 => {
                    self.report.sps_count += 1;
                    self.report.has_sps = true;
                    params.sps = Some(nalu.to_vec());
                }
                8 => {
                    self.report.pps_count += 1;
                    self.report.has_pps = true;
                    params.pps = Some(nalu.to_vec());
                }
                _ => {}
            },
            VideoCodecKind::H265 if nalu.len() >= 2 => match (nalu[0] >> 1) & 0x3F {
                32 => {
                    self.report.vps_count += 1;
                    self.report.has_vps = true;
                    params.vps = Some(nalu.to_vec());
                }
                33 => {
                    self.report.sps_count += 1;
                    self.report.has_sps = true;
                    params.sps = Some(nalu.to_vec());
                }
                34 => {
                    self.report.pps_count += 1;
                    self.report.has_pps = true;
                    params.pps = Some(nalu.to_vec());
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn write_video_annexb(&mut self, codec: VideoCodecKind, nalus: &[&[u8]]) -> anyhow::Result<()> {
        if self.video_file.is_none() {
            let path = self
                .capture_dir
                .join(format!("video.{}", codec.extension()));
            let file = File::create(&path)?;
            self.video_file = Some(BufWriter::new(file));
            self.video_path = Some(path);
        }
        for nalu in nalus {
            let needed = u64::try_from(4 + nalu.len()).context("NALU size")?;
            if self.bytes_written.saturating_add(needed) > self.max_bytes {
                return Ok(());
            }
            if let Some(file) = self.video_file.as_mut() {
                file.write_all(&ANNEXB_START_CODE)?;
                file.write_all(nalu)?;
                self.bytes_written += needed;
            }
        }
        Ok(())
    }

    fn write_audio_elementary(
        &mut self,
        codec: AudioElementaryCodec,
        bytes: &[u8],
    ) -> anyhow::Result<()> {
        if self.audio_file.is_none() {
            let path = self
                .capture_dir
                .join(format!("audio.{}", codec.extension()));
            let file = File::create(&path)?;
            self.audio_file = Some(BufWriter::new(file));
            self.audio_path = Some(path);
        }
        let needed = u64::try_from(bytes.len()).context("audio frame size")?;
        if self.bytes_written.saturating_add(needed) > self.max_bytes {
            return Ok(());
        }
        if let Some(file) = self.audio_file.as_mut() {
            file.write_all(bytes)?;
            self.bytes_written += needed;
        }
        Ok(())
    }

    fn append_rtp_dump(
        &mut self,
        track: &str,
        channel: Option<u8>,
        timestamp_us: u64,
        raw_rtp: &[u8],
    ) -> anyhow::Result<()> {
        if self.rtp_dump_file.is_none() {
            let path = self.capture_dir.join("rtp_packets.bin");
            let file = File::create(&path)?;
            self.rtp_dump_file = Some(BufWriter::new(file));
            self.rtp_dump_path = Some(path);
        }
        let track_bytes = track.as_bytes();
        let track_len = u8::try_from(track_bytes.len().min(255)).unwrap_or(255);
        let rtp_len = u32::try_from(raw_rtp.len()).context("RTP packet length")?;
        let needed = 1 + usize::from(track_len) + 2 + 8 + 4 + raw_rtp.len();
        if self.bytes_written.saturating_add(needed as u64) > self.max_bytes {
            return Ok(());
        }
        if let Some(file) = self.rtp_dump_file.as_mut() {
            file.write_all(&[track_len])?;
            file.write_all(&track_bytes[..usize::from(track_len)])?;
            file.write_all(&[u8::from(channel.is_some()), channel.unwrap_or(0)])?;
            file.write_all(&timestamp_us.to_be_bytes())?;
            file.write_all(&rtp_len.to_be_bytes())?;
            file.write_all(raw_rtp)?;
            self.bytes_written += needed as u64;
        }
        Ok(())
    }

    fn push_mp4_sample(&mut self, sample: PendingSample) -> anyhow::Result<()> {
        if self.mp4_failed {
            return Ok(());
        }
        if self.mp4_writer.is_some() {
            return self.write_mp4_sample(sample);
        }
        if self.pending_mp4.len() < MAX_PENDING_MP4_SAMPLES
            && self.pending_mp4_bytes.saturating_add(sample.bytes.len()) <= MAX_PENDING_MP4_BYTES
        {
            self.pending_mp4_bytes += sample.bytes.len();
            self.pending_mp4.push(sample);
        }
        self.try_init_mp4()
    }

    fn try_init_mp4(&mut self) -> anyhow::Result<()> {
        if self.mp4_writer.is_some() || self.mp4_failed {
            return Ok(());
        }
        let Some(video_track) = self.build_mp4_video_track() else {
            return Ok(());
        };
        let path = self.capture_dir.join("muxed.mp4");
        let file = File::create(&path)?;
        let config = Mp4Config {
            major_brand: "isom".parse().expect("valid fourcc"),
            minor_version: 512,
            compatible_brands: vec![
                "isom".parse().expect("valid fourcc"),
                "iso2".parse().expect("valid fourcc"),
                "mp41".parse().expect("valid fourcc"),
            ],
            timescale: DEFAULT_MP4_TIMESCALE,
        };
        let mut writer = Mp4Writer::write_start(BufWriter::new(file), &config)?;
        writer.add_track(&video_track)?;
        self.mp4_video_track_id = Some(1);
        if let Some(audio_track) = self.build_mp4_audio_track() {
            writer.add_track(&audio_track)?;
            self.mp4_audio_track_id = Some(2);
        }
        self.mp4_writer = Some(writer);
        self.mp4_path = Some(path);
        let pending = std::mem::take(&mut self.pending_mp4);
        self.pending_mp4_bytes = 0;
        for sample in pending {
            self.write_mp4_sample(sample)?;
        }
        Ok(())
    }

    fn build_mp4_video_track(&self) -> Option<TrackConfig> {
        let params = self.video_params.as_ref()?;
        let media_conf = match params.codec {
            VideoCodecKind::H264 => MediaConfig::AvcConfig(AvcConfig {
                width: params.width.max(1),
                height: params.height.max(1),
                seq_param_set: params.sps.clone()?,
                pic_param_set: params.pps.clone()?,
            }),
            VideoCodecKind::H265 => MediaConfig::HevcConfig(HevcConfig {
                width: params.width.max(1),
                height: params.height.max(1),
                vps: params.vps.clone()?,
                sps: params.sps.clone()?,
                pps: params.pps.clone()?,
                decoder_config: Vec::new(),
            }),
        };
        Some(TrackConfig {
            track_type: TrackType::Video,
            timescale: DEFAULT_MP4_TIMESCALE,
            language: "und".to_owned(),
            media_conf,
        })
    }

    fn build_mp4_audio_track(&self) -> Option<TrackConfig> {
        let params = self.audio_params?;
        if params.codec != AudioElementaryCodec::Aac {
            return None;
        }
        let freq_index = sample_freq_index(params.sample_rate_hz)?;
        let chan_conf = ChannelConfig::try_from(params.channels.max(1)).ok()?;
        let profile = match params.aac_profile {
            1 => AudioObjectType::AacMain,
            3 => AudioObjectType::AacScalableSampleRate,
            4 => AudioObjectType::AacLongTermPrediction,
            _ => AudioObjectType::AacLowComplexity,
        };
        Some(TrackConfig {
            track_type: TrackType::Audio,
            timescale: DEFAULT_MP4_TIMESCALE,
            language: "und".to_owned(),
            media_conf: MediaConfig::AacConfig(AacConfig {
                bitrate: 64_000,
                profile,
                freq_index,
                chan_conf,
            }),
        })
    }

    fn write_mp4_sample(&mut self, sample: PendingSample) -> anyhow::Result<()> {
        let track_id = if sample.is_audio {
            self.mp4_audio_track_id
        } else {
            self.mp4_video_track_id
        };
        let Some(track_id) = track_id else {
            return Ok(());
        };
        let needed = u64::try_from(sample.bytes.len()).context("MP4 sample size")?;
        if self.bytes_written.saturating_add(needed) > self.max_bytes {
            return Ok(());
        }
        if let Some(writer) = self.mp4_writer.as_mut() {
            let res = writer.write_sample(
                track_id,
                &Mp4Sample {
                    start_time: sample.pts_90k,
                    duration: sample.duration_90k,
                    rendering_offset: 0,
                    is_sync: sample.is_sync,
                    bytes: sample.bytes,
                },
            );
            if res.is_err() {
                self.mp4_failed = true;
                return Ok(());
            }
            self.bytes_written += needed;
        }
        Ok(())
    }
}

struct ParsedRtp<'a> {
    marker: bool,
    payload_type: u8,
    sequence: u16,
    timestamp: u32,
    ssrc: u32,
    payload: &'a [u8],
}

fn parse_rtp_packet(data: &[u8]) -> anyhow::Result<ParsedRtp<'_>> {
    ensure!(data.len() >= RTP_MIN_HEADER_LEN, "RTP packet too short");
    ensure!((data[0] >> 6) == 2, "unsupported RTP version");
    let has_padding = (data[0] & 0x20) != 0;
    let has_extension = (data[0] & 0x10) != 0;
    let csrc_count = usize::from(data[0] & 0x0F);
    let mut offset = RTP_MIN_HEADER_LEN + csrc_count * 4;
    ensure!(offset <= data.len(), "RTP CSRC list exceeds packet");
    if has_extension {
        ensure!(offset + 4 <= data.len(), "RTP extension header truncated");
        let ext_words = usize::from(u16::from_be_bytes([data[offset + 2], data[offset + 3]]));
        offset += 4 + ext_words * 4;
        ensure!(offset <= data.len(), "RTP extension body exceeds packet");
    }
    let end = if has_padding {
        ensure!(offset < data.len(), "RTP padding byte missing");
        let pad = usize::from(data[data.len() - 1]);
        ensure!(pad > 0 && pad <= data.len() - offset, "invalid RTP padding");
        data.len() - pad
    } else {
        data.len()
    };
    Ok(ParsedRtp {
        marker: (data[1] & 0x80) != 0,
        payload_type: data[1] & 0x7F,
        sequence: u16::from_be_bytes([data[2], data[3]]),
        timestamp: u32::from_be_bytes([data[4], data[5], data[6], data[7]]),
        ssrc: u32::from_be_bytes([data[8], data[9], data[10], data[11]]),
        payload: &data[offset..end],
    })
}

const fn check_rtp_sequence(state: &mut FuaTrackState, seq: u16) -> bool {
    let gap = match state.last_seq {
        Some(prev) => prev.wrapping_add(1) != seq,
        None => false,
    };
    state.last_seq = Some(seq);
    gap
}

fn update_fu_state(
    state: &mut FuaTrackState,
    codec: VideoCodecKind,
    payload: &[u8],
    _marker: bool,
) -> (u8, u8) {
    if payload.is_empty() {
        return (0, 0);
    }
    match codec {
        VideoCodecKind::H264 => {
            let nal_type = payload[0] & 0x1F;
            if nal_type != 28 {
                let end_err = u8::from(state.in_fragment);
                state.in_fragment = false;
                return (0, end_err);
            }
            if payload.len() < 2 {
                return (1, 0);
            }
            apply_fu_bits(state, (payload[1] & 0x80) != 0, (payload[1] & 0x40) != 0)
        }
        VideoCodecKind::H265 => {
            let nal_type = (payload[0] >> 1) & 0x3F;
            if nal_type != 49 {
                let end_err = u8::from(state.in_fragment);
                state.in_fragment = false;
                return (0, end_err);
            }
            if payload.len() < 3 {
                return (1, 0);
            }
            apply_fu_bits(state, (payload[2] & 0x80) != 0, (payload[2] & 0x40) != 0)
        }
    }
}

const fn apply_fu_bits(state: &mut FuaTrackState, start: bool, end: bool) -> (u8, u8) {
    let mut start_err = 0_u8;
    let mut end_err = 0_u8;
    if start && end {
        start_err = 1;
        state.in_fragment = false;
        return (start_err, end_err);
    }
    if start {
        if state.in_fragment {
            end_err = 1;
        }
        state.in_fragment = true;
    } else if !state.in_fragment {
        start_err = 1;
    } else if end {
        state.in_fragment = false;
    }
    (start_err, end_err)
}

fn collect_avcc_nalus<'a>(avcc: &'a [u8], out: &mut Vec<&'a [u8]>) {
    let mut pos = 0;
    while pos + 4 <= avcc.len() {
        let len =
            u32::from_be_bytes([avcc[pos], avcc[pos + 1], avcc[pos + 2], avcc[pos + 3]]) as usize;
        pos += 4;
        if len == 0 || pos + len > avcc.len() {
            break;
        }
        out.push(&avcc[pos..pos + len]);
        pos += len;
    }
}

fn filter_mp4_video_nalus(codec: VideoCodecKind, nalus: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for nalu in nalus {
        if nalu.is_empty() {
            continue;
        }
        let is_param = match codec {
            VideoCodecKind::H264 => matches!(nalu[0] & 0x1F, 7 | 8),
            VideoCodecKind::H265 => matches!((nalu[0] >> 1) & 0x3F, 32..=34),
        };
        if !is_param {
            out.extend_from_slice(&(nalu.len() as u32).to_be_bytes());
            out.extend_from_slice(nalu);
        }
    }
    out
}

fn ensure_adts_frame(params: AudioStreamParams, frame: &[u8]) -> Vec<u8> {
    if parse_adts(frame).is_some() {
        return frame.to_vec();
    }
    let freq_idx = sample_freq_index(params.sample_rate_hz).map_or(4, |idx| idx as u8);
    let profile_minus_one = params.aac_profile.saturating_sub(1).min(3);
    let channels = params.channels.clamp(1, 7);
    let total_len = 7 + frame.len();
    let header = [
        0xFF,
        0xF1,
        (profile_minus_one << 6) | ((freq_idx & 0x0F) << 2) | ((channels >> 2) & 0x01),
        ((channels & 0x03) << 6) | (((total_len >> 11) & 0x03) as u8),
        ((total_len >> 3) & 0xFF) as u8,
        (((total_len & 0x07) as u8) << 5) | 0x1F,
        0xFC,
    ];
    let mut out = Vec::with_capacity(total_len);
    out.extend_from_slice(&header);
    out.extend_from_slice(frame);
    out
}

const fn sample_freq_index(rate_hz: u32) -> Option<SampleFreqIndex> {
    match rate_hz {
        96_000 => Some(SampleFreqIndex::Freq96000),
        88_200 => Some(SampleFreqIndex::Freq88200),
        64_000 => Some(SampleFreqIndex::Freq64000),
        48_000 => Some(SampleFreqIndex::Freq48000),
        44_100 => Some(SampleFreqIndex::Freq44100),
        32_000 => Some(SampleFreqIndex::Freq32000),
        24_000 => Some(SampleFreqIndex::Freq24000),
        22_050 => Some(SampleFreqIndex::Freq22050),
        16_000 => Some(SampleFreqIndex::Freq16000),
        12_000 => Some(SampleFreqIndex::Freq12000),
        11_025 => Some(SampleFreqIndex::Freq11025),
        8_000 => Some(SampleFreqIndex::Freq8000),
        7_350 => Some(SampleFreqIndex::Freq7350),
        _ => None,
    }
}

const fn audio_duration_90k(sample_rate_hz: u32) -> u32 {
    if sample_rate_hz == 0 {
        return 1920;
    }
    ((1024_u64 * 90_000) / sample_rate_hz as u64) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("keeppeek-bitstream-{}", uuid::Uuid::new_v4()));
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

    fn make_rtp(seq: u16, ts: u32, marker: bool, payload: &[u8]) -> Vec<u8> {
        let mut pkt = Vec::with_capacity(12 + payload.len());
        pkt.push(0x80);
        pkt.push(if marker { 0x80 | 96 } else { 96 });
        pkt.extend_from_slice(&seq.to_be_bytes());
        pkt.extend_from_slice(&ts.to_be_bytes());
        pkt.extend_from_slice(&0x1234_5678_u32.to_be_bytes());
        pkt.extend_from_slice(payload);
        pkt
    }

    #[test]
    fn writes_clean_h264_and_aac_with_muxed_mp4_and_zero_anomalies() {
        let dir = TempDir::new();
        let sps = STANDARD
            .decode("Z00AKZpkA8ARPyzUBAQFAAADA+gAAOpgBA==")
            .unwrap();
        let pps = STANDARD.decode("aO48gA==").unwrap();
        let mut rec = BitstreamRecorder::new(dir.path(), 8 * 1024 * 1024, true).unwrap();
        let audio_params = AudioStreamParams {
            codec: AudioElementaryCodec::Aac,
            sample_rate_hz: 16_000,
            channels: 1,
            aac_profile: 2,
        };
        rec.configure_video(VideoStreamParams {
            codec: VideoCodecKind::H264,
            width: 640,
            height: 360,
            sps: Some(sps.clone()),
            pps: Some(pps.clone()),
            vps: None,
        });
        rec.configure_audio(audio_params);
        feed_clean_h264_and_aac(&mut rec, &sps, &pps, audio_params);
        let out = rec.finish().unwrap();
        assert!(out.video_path.as_ref().unwrap().exists());
        assert!(out.audio_path.as_ref().unwrap().exists());
        assert!(out.muxed_mp4_path.as_ref().unwrap().exists());
        assert!(out.rtp_dump_path.as_ref().unwrap().exists());
        assert!(parse_adts(&std::fs::read(out.audio_path.unwrap()).unwrap()).is_some());
        let reader = mp4::read_mp4(File::open(out.muxed_mp4_path.unwrap()).unwrap()).unwrap();
        assert_eq!(reader.tracks().len(), 2);
        let a = out.anomalies;
        assert!(a.has_sps && a.has_pps && !a.missing_idr);
        assert_eq!(
            (a.idr_count, a.fua_start_errors, a.fua_end_errors),
            (1, 0, 0)
        );
        assert_eq!((a.rtp_seq_gaps, a.dts_pts_regressions), (0, 0));
        assert_eq!(a.av_drift_ms_latest, Some(20));
        assert_eq!(a.av_drift_ms_max, Some(33));
    }

    fn feed_clean_h264_and_aac(
        rec: &mut BitstreamRecorder,
        sps: &[u8],
        pps: &[u8],
        audio_params: AudioStreamParams,
    ) {
        for (seq, ts, marker, payload) in [
            (10, 90_000, true, &[0x65, 0x88, 0x84, 0x21][..]),
            (11, 93_000, false, &[0x7C, 0x81, 0xAA][..]),
            (12, 93_000, true, &[0x7C, 0x41, 0xBB][..]),
        ] {
            let rtp = make_rtp(seq, ts, marker, payload);
            rec.record_rtp_packet("video", Some(0), Some(VideoCodecKind::H264), 1000, &rtp)
                .unwrap();
        }
        let mut frame1 = Vec::new();
        for nalu in [sps, pps, &[0x65, 0x88, 0x84, 0x21]] {
            frame1.extend_from_slice(&ANNEXB_START_CODE);
            frame1.extend_from_slice(nalu);
        }
        rec.record_video_frame(VideoCodecKind::H264, VideoFraming::AnnexB, 0, true, &frame1)
            .unwrap();
        rec.record_video_frame(
            VideoCodecKind::H264,
            VideoFraming::AnnexB,
            33_000,
            false,
            &[0, 0, 0, 1, 0x41, 0x9A, 0x01],
        )
        .unwrap();
        rec.record_audio_frame(audio_params, 0, &[0x21, 0x10, 0x04, 0x60])
            .unwrap();
        rec.record_audio_frame(audio_params, 53_000, &[0x21, 0x10, 0x04, 0x60])
            .unwrap();
    }

    #[test]
    fn detects_broken_fua_seq_gaps_dts_regression_and_missing_idr_in_h264_and_h265() {
        let dir = TempDir::new();
        let mut rec = BitstreamRecorder::new(dir.path(), 4 * 1024 * 1024, false).unwrap();

        let bad_mid = make_rtp(1, 1000, false, &[0x7C, 0x01, 0xAA]);
        let start_only = make_rtp(2, 1000, false, &[0x7C, 0x81, 0xBB]);
        let gap_end = make_rtp(4, 900, true, &[0x7C, 0x41, 0xCC]);
        let single_se = make_rtp(5, 1100, true, &[0x7C, 0xC1, 0xDD]);
        rec.record_rtp_packet("v4", None, Some(VideoCodecKind::H264), 100, &bad_mid)
            .unwrap();
        rec.record_rtp_packet("v4", None, Some(VideoCodecKind::H264), 200, &start_only)
            .unwrap();
        rec.record_rtp_packet("v4", None, Some(VideoCodecKind::H264), 300, &gap_end)
            .unwrap();
        rec.record_rtp_packet("v4", None, Some(VideoCodecKind::H264), 400, &single_se)
            .unwrap();

        let hevc_fu_unterminated = make_rtp(10, 2000, false, &[0x62, 0x01, 0x81, 0xEE]);
        rec.record_rtp_packet(
            "v5",
            None,
            Some(VideoCodecKind::H265),
            500,
            &hevc_fu_unterminated,
        )
        .unwrap();

        rec.record_video_frame(
            VideoCodecKind::H265,
            VideoFraming::AnnexB,
            50_000,
            false,
            &[0, 0, 0, 1, 0x02, 0x01, 0xAA],
        )
        .unwrap();
        rec.record_video_frame(
            VideoCodecKind::H265,
            VideoFraming::AnnexB,
            40_000,
            false,
            &[0, 0, 0, 1, 0x02, 0x01, 0xBB],
        )
        .unwrap();

        let out = rec.finish().unwrap();
        let a = out.anomalies;
        assert!(a.missing_idr);
        assert_eq!(a.idr_count, 0);
        assert_eq!(a.rtp_seq_gaps, 1);
        assert_eq!(a.dts_pts_regressions, 2);
        assert_eq!(a.fua_start_errors, 3);
        assert_eq!(a.fua_end_errors, 2);
    }

    #[test]
    fn writes_g711_and_opus_elementary_files() {
        let dir = TempDir::new();
        let mut rec = BitstreamRecorder::new(dir.path(), 1024 * 1024, false).unwrap();
        let pcmu = AudioStreamParams {
            codec: AudioElementaryCodec::Pcmu,
            sample_rate_hz: 8_000,
            channels: 1,
            aac_profile: 0,
        };
        rec.record_audio_frame(pcmu, 0, &[0x7F, 0x80]).unwrap();
        let opus = AudioStreamParams {
            codec: AudioElementaryCodec::Opus,
            sample_rate_hz: 48_000,
            channels: 2,
            aac_profile: 0,
        };
        rec.record_audio_frame(opus, 20_000, &[0xFC, 0x01, 0x02])
            .unwrap();
        let out = rec.finish().unwrap();
        assert!(out.audio_path.unwrap().exists());
        assert_eq!(out.anomalies.audio_frames, 2);
    }
}
