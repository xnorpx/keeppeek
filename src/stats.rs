//! Cumulative ingress statistics — all counters are monotonically increasing.
//! Diff two [`IngressSnapshot`]s to compute rates over any window.

use crate::{cameras::VideoEncoding, keeppeek::StreamKind};
use hdrhistogram::Histogram;
use serde::Serialize;
use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub(crate) const REPORT_INTERVAL: Duration = Duration::from_secs(10);
const FRAME_JITTER_MAX_US: u64 = 5 * 60 * 1_000_000;

struct FrameJitterHistogram {
    histogram: Histogram<u64>,
}

impl Default for FrameJitterHistogram {
    fn default() -> Self {
        Self {
            histogram: Histogram::new_with_max(FRAME_JITTER_MAX_US, 3)
                .expect("valid frame jitter histogram configuration"),
        }
    }
}

impl FrameJitterHistogram {
    fn record(&mut self, frame_gap_us: u64, expected_fps: f64) {
        if !expected_fps.is_finite() || expected_fps <= 0.0 {
            return;
        }
        let expected_gap_us = (1_000_000.0 / expected_fps).round() as u64;
        self.histogram
            .saturating_record(frame_gap_us.abs_diff(expected_gap_us));
    }

    fn snapshot_and_reset(&mut self) -> FrameJitterSnapshot {
        let snapshot = FrameJitterSnapshot {
            samples: self.histogram.len(),
            p50_us: self.histogram.value_at_quantile(0.50),
            p99_us: self.histogram.value_at_quantile(0.99),
        };
        self.histogram.reset();
        snapshot
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct FrameJitterSnapshot {
    samples: u64,
    p50_us: u64,
    p99_us: u64,
}

pub(crate) struct IngressStats {
    created: Instant,

    pub reconnects: u64,

    pub video_frames: u64,
    pub video_keyframes: u64,
    pub video_bytes: u64,
    pub video_max_frame: usize,
    pub video_gap_sum_us: u64,
    pub video_gap_count: u64,
    video_gap_min_us: u64,
    video_gap_max_us: u64,
    last_video_frame: Option<Instant>,
    video_jitter: FrameJitterHistogram,

    pub audio_frames: u64,
    pub audio_bytes: u64,
    pub audio_max_frame: usize,

    pub dropped_frames: u64,
    pub error_count: u64,

    pub codec: Option<VideoEncoding>,
    pub width: u32,
    pub height: u32,
    pub expected_fps: f64,
}

#[derive(Debug, Clone)]
pub(crate) struct IngressSnapshot {
    pub uptime_secs: f64,
    pub reconnects: u64,

    pub video_frames: u64,
    pub video_keyframes: u64,
    pub video_bytes: u64,
    pub video_max_frame: usize,

    pub audio_frames: u64,
    pub audio_bytes: u64,
    pub audio_max_frame: usize,

    pub dropped_frames: u64,
    pub error_count: u64,

    pub gap_min_ms: f64,
    pub gap_max_ms: f64,
    pub gap_avg_ms: f64,
    pub jitter_samples: u64,
    pub jitter_p50_ms: f64,
    pub jitter_p99_ms: f64,
}

impl IngressSnapshot {
    pub fn rates_since(&self, prev: &Self) -> IngressRates {
        let dt = self.uptime_secs - prev.uptime_secs;
        let dt = if dt > 0.0 { dt } else { 1.0 };

        let d_video = self.video_frames.saturating_sub(prev.video_frames);
        let d_kf = self.video_keyframes.saturating_sub(prev.video_keyframes);
        let d_vbytes = self.video_bytes.saturating_sub(prev.video_bytes);
        let d_audio = self.audio_frames.saturating_sub(prev.audio_frames);
        let d_abytes = self.audio_bytes.saturating_sub(prev.audio_bytes);

        IngressRates {
            video_fps: d_video as f64 / dt,
            keyframe_fps: d_kf as f64 / dt,
            video_kbps: d_vbytes as f64 * 8.0 / 1024.0 / dt,
            audio_fps: d_audio as f64 / dt,
            audio_kbps: d_abytes as f64 * 8.0 / 1024.0 / dt,
            gap_min_ms: self.gap_min_ms,
            gap_max_ms: self.gap_max_ms,
            gap_avg_ms: self.gap_avg_ms,
        }
    }
}

pub(crate) struct IngressRates {
    pub video_fps: f64,
    pub keyframe_fps: f64,
    pub video_kbps: f64,
    pub audio_fps: f64,
    pub audio_kbps: f64,
    pub gap_min_ms: f64,
    pub gap_max_ms: f64,
    pub gap_avg_ms: f64,
}

impl IngressStats {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            created: now,
            reconnects: 0,
            video_frames: 0,
            video_keyframes: 0,
            video_bytes: 0,
            video_max_frame: 0,
            video_gap_sum_us: 0,
            video_gap_count: 0,
            video_gap_min_us: u64::MAX,
            video_gap_max_us: 0,
            last_video_frame: None,
            video_jitter: FrameJitterHistogram::default(),
            audio_frames: 0,
            audio_bytes: 0,
            audio_max_frame: 0,
            dropped_frames: 0,
            error_count: 0,
            codec: None,
            width: 0,
            height: 0,
            expected_fps: 0.0,
        }
    }

    pub fn on_video_frame(&mut self, is_keyframe: bool, bytes: usize) {
        let now = Instant::now();
        if let Some(prev) = self.last_video_frame {
            let gap_us = now.duration_since(prev).as_micros() as u64;
            self.video_gap_sum_us = self.video_gap_sum_us.wrapping_add(gap_us);
            self.video_gap_count = self.video_gap_count.wrapping_add(1);
            if gap_us < self.video_gap_min_us {
                self.video_gap_min_us = gap_us;
            }
            if gap_us > self.video_gap_max_us {
                self.video_gap_max_us = gap_us;
            }
            self.video_jitter.record(gap_us, self.expected_fps);
        }
        self.last_video_frame = Some(now);
        self.video_frames = self.video_frames.wrapping_add(1);
        if is_keyframe {
            self.video_keyframes = self.video_keyframes.wrapping_add(1);
        }
        self.video_bytes = self.video_bytes.wrapping_add(bytes as u64);
        if bytes > self.video_max_frame {
            self.video_max_frame = bytes;
        }
    }

    pub const fn on_audio_frame(&mut self, bytes: usize) {
        self.audio_frames = self.audio_frames.wrapping_add(1);
        self.audio_bytes = self.audio_bytes.wrapping_add(bytes as u64);
        if bytes > self.audio_max_frame {
            self.audio_max_frame = bytes;
        }
    }

    pub const fn on_connect(&mut self) {
        self.reconnects = self.reconnects.wrapping_add(1);
    }

    #[expect(dead_code)]
    pub const fn on_drop(&mut self) {
        self.dropped_frames = self.dropped_frames.wrapping_add(1);
    }

    pub const fn on_error(&mut self) {
        self.error_count = self.error_count.wrapping_add(1);
    }

    pub fn set_stream_info(
        &mut self,
        codec: VideoEncoding,
        width: u32,
        height: u32,
        expected_fps: f64,
    ) {
        self.codec = Some(codec);
        self.width = width;
        self.height = height;
        self.expected_fps = expected_fps;
    }

    pub fn snapshot(&mut self) -> IngressSnapshot {
        let now = Instant::now();
        let uptime = now.duration_since(self.created).as_secs_f64();

        let gap_avg_ms = if self.video_gap_count > 0 {
            self.video_gap_sum_us as f64 / self.video_gap_count as f64 / 1000.0
        } else {
            0.0
        };
        let gap_min_ms = if self.video_gap_min_us < u64::MAX {
            self.video_gap_min_us as f64 / 1000.0
        } else {
            0.0
        };
        let gap_max_ms = self.video_gap_max_us as f64 / 1000.0;
        let jitter = self.video_jitter.snapshot_and_reset();

        let snap = IngressSnapshot {
            uptime_secs: uptime,
            reconnects: self.reconnects,
            video_frames: self.video_frames,
            video_keyframes: self.video_keyframes,
            video_bytes: self.video_bytes,
            video_max_frame: self.video_max_frame,
            audio_frames: self.audio_frames,
            audio_bytes: self.audio_bytes,
            audio_max_frame: self.audio_max_frame,
            dropped_frames: self.dropped_frames,
            error_count: self.error_count,
            gap_min_ms,
            gap_max_ms,
            gap_avg_ms,
            jitter_samples: jitter.samples,
            jitter_p50_ms: jitter.p50_us as f64 / 1000.0,
            jitter_p99_ms: jitter.p99_us as f64 / 1000.0,
        };

        self.video_gap_min_us = u64::MAX;
        self.video_gap_max_us = 0;

        snap
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CameraReport {
    pub ip: IpAddr,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brand: Option<String>,
    pub port: u16,
    pub streams: Vec<StreamReport>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct StreamReport {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(skip_serializing_if = "is_zero_f64")]
    pub fps: f64,
    #[serde(skip_serializing_if = "is_zero_f64")]
    pub expected_fps: f64,
    #[serde(skip_serializing_if = "is_zero_f64")]
    pub kf_fps: f64,
    #[serde(skip_serializing_if = "is_zero_f64")]
    pub kbps: f64,
    #[serde(skip_serializing_if = "is_zero_f64")]
    pub max_frame_kb: f64,
    #[serde(skip_serializing_if = "is_zero_f64")]
    pub gap_min_ms: f64,
    #[serde(skip_serializing_if = "is_zero_f64")]
    pub gap_avg_ms: f64,
    #[serde(skip_serializing_if = "is_zero_f64")]
    pub gap_max_ms: f64,
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub jitter_samples: u64,
    #[serde(skip_serializing_if = "is_zero_f64")]
    pub jitter_p50_ms: f64,
    #[serde(skip_serializing_if = "is_zero_f64")]
    pub jitter_p99_ms: f64,
    #[serde(skip_serializing_if = "is_none_or_zero_u64")]
    pub frames: Option<u64>,
    #[serde(skip_serializing_if = "is_none_or_zero_u64")]
    pub bytes: Option<u64>,
    #[serde(skip_serializing_if = "is_none_or_zero_u64")]
    pub keyframes: Option<u64>,
    #[serde(skip_serializing_if = "is_none_or_zero_u64")]
    pub reconnects: Option<u64>,
    #[serde(skip_serializing_if = "is_none_or_zero_u64")]
    pub drops: Option<u64>,
    #[serde(skip_serializing_if = "is_none_or_zero_u64")]
    pub errors: Option<u64>,
}

#[derive(Clone, Default)]
pub struct HealthRegistry {
    inner: Arc<Mutex<HashMap<IpAddr, RegisteredCamera>>>,
}

struct RegisteredCamera {
    name: Option<String>,
    brand: Option<String>,
    port: u16,
    streams: HashMap<String, RegisteredStream>,
}

struct RegisteredStream {
    report: StreamReport,
    updated_at: Instant,
    updated_at_ms: u64,
    frame_updated_at: Option<Instant>,
    frame_updated_at_ms: Option<u64>,
    keyframe_updated_at: Option<Instant>,
    keyframe_updated_at_ms: Option<u64>,
    recent_reconnects: u64,
    recent_drops: u64,
    recent_errors: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CameraHealthReport {
    pub ip: IpAddr,
    pub name: Option<String>,
    pub brand: Option<String>,
    pub port: u16,
    pub streams: Vec<StreamHealthReport>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct StreamHealthReport {
    #[serde(flatten)]
    pub report: StreamReport,
    pub updated_at_ms: u64,
    pub report_age_ms: u64,
    pub frame_updated_at_ms: Option<u64>,
    pub frame_age_ms: Option<u64>,
    pub keyframe_updated_at_ms: Option<u64>,
    pub keyframe_age_ms: Option<u64>,
    pub recent_reconnects: u64,
    pub recent_drops: u64,
    pub recent_errors: u64,
}

impl HealthRegistry {
    /// Creates an empty stream-health registry.
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn publish(&self, report: CameraReport) {
        self.publish_at(report, Instant::now(), unix_time_ms());
    }

    fn publish_at(&self, report: CameraReport, now: Instant, updated_at_ms: u64) {
        let mut cameras = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let camera = cameras
            .entry(report.ip)
            .or_insert_with(|| RegisteredCamera {
                name: report.name.clone(),
                brand: report.brand.clone(),
                port: report.port,
                streams: HashMap::new(),
            });
        if report.name.is_some() {
            camera.name = report.name;
        }
        if report.brand.is_some() {
            camera.brand = report.brand;
        }
        camera.port = report.port;
        for stream in report.streams {
            let previous = camera.streams.get(&stream.kind);
            let frames = stream.frames.unwrap_or(0);
            let keyframes = stream.keyframes.unwrap_or(0);
            let frame_progressed = counter_progressed(
                previous.map(|entry| entry.report.frames.unwrap_or(0)),
                frames,
            );
            let keyframe_progressed = counter_progressed(
                previous.map(|entry| entry.report.keyframes.unwrap_or(0)),
                keyframes,
            );
            let frame_updated_at = if frame_progressed {
                Some(now)
            } else {
                previous.and_then(|entry| entry.frame_updated_at)
            };
            let frame_updated_at_ms = if frame_progressed {
                Some(updated_at_ms)
            } else {
                previous.and_then(|entry| entry.frame_updated_at_ms)
            };
            let keyframe_updated_at = if keyframe_progressed {
                Some(now)
            } else {
                previous.and_then(|entry| entry.keyframe_updated_at)
            };
            let keyframe_updated_at_ms = if keyframe_progressed {
                Some(updated_at_ms)
            } else {
                previous.and_then(|entry| entry.keyframe_updated_at_ms)
            };
            let recent_reconnects = counter_delta(
                previous.map(|entry| entry.report.reconnects.unwrap_or(0)),
                stream.reconnects.unwrap_or(0),
            );
            let recent_drops = counter_delta(
                previous.map(|entry| entry.report.drops.unwrap_or(0)),
                stream.drops.unwrap_or(0),
            );
            let recent_errors = counter_delta(
                previous.map(|entry| entry.report.errors.unwrap_or(0)),
                stream.errors.unwrap_or(0),
            );
            camera.streams.insert(
                stream.kind.clone(),
                RegisteredStream {
                    report: stream,
                    updated_at: now,
                    updated_at_ms,
                    frame_updated_at,
                    frame_updated_at_ms,
                    keyframe_updated_at,
                    keyframe_updated_at_ms,
                    recent_reconnects,
                    recent_drops,
                    recent_errors,
                },
            );
        }
    }

    pub(crate) fn snapshot(&self) -> Vec<CameraHealthReport> {
        self.snapshot_at(Instant::now())
    }

    fn snapshot_at(&self, now: Instant) -> Vec<CameraHealthReport> {
        let cameras = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut reports = cameras
            .iter()
            .map(|(ip, camera)| {
                let mut streams = camera
                    .streams
                    .values()
                    .map(|stream| StreamHealthReport {
                        report: stream.report.clone(),
                        updated_at_ms: stream.updated_at_ms,
                        report_age_ms: now
                            .saturating_duration_since(stream.updated_at)
                            .as_millis()
                            .try_into()
                            .unwrap_or(u64::MAX),
                        frame_updated_at_ms: stream.frame_updated_at_ms,
                        frame_age_ms: stream.frame_updated_at.map(|updated_at| {
                            now.saturating_duration_since(updated_at)
                                .as_millis()
                                .try_into()
                                .unwrap_or(u64::MAX)
                        }),
                        keyframe_updated_at_ms: stream.keyframe_updated_at_ms,
                        keyframe_age_ms: stream.keyframe_updated_at.map(|updated_at| {
                            now.saturating_duration_since(updated_at)
                                .as_millis()
                                .try_into()
                                .unwrap_or(u64::MAX)
                        }),
                        recent_reconnects: stream.recent_reconnects,
                        recent_drops: stream.recent_drops,
                        recent_errors: stream.recent_errors,
                    })
                    .collect::<Vec<_>>();
                streams.sort_unstable_by(|left, right| left.report.kind.cmp(&right.report.kind));
                CameraHealthReport {
                    ip: *ip,
                    name: camera.name.clone(),
                    brand: camera.brand.clone(),
                    port: camera.port,
                    streams,
                }
            })
            .collect::<Vec<_>>();
        reports.sort_unstable_by_key(|report| report.ip);
        reports
    }

    pub(crate) fn remove(&self, camera_ip: IpAddr) {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&camera_ip);
    }
}

fn counter_progressed(previous: Option<u64>, current: u64) -> bool {
    current > 0 && previous != Some(current)
}

fn counter_delta(previous: Option<u64>, current: u64) -> u64 {
    previous.map_or(current, |previous| {
        if current >= previous {
            current - previous
        } else {
            current
        }
    })
}

pub(crate) fn video_report(
    kind: StreamKind,
    snap: &IngressSnapshot,
    rates: &IngressRates,
    codec: Option<&VideoEncoding>,
    width: u32,
    height: u32,
    expected_fps: f64,
) -> StreamReport {
    StreamReport {
        kind: match kind {
            StreamKind::Main => "video_main".into(),
            StreamKind::Sub => "video_sub".into(),
        },
        codec: codec.map(|c| c.to_string()),
        resolution: if width > 0 && height > 0 {
            Some(format!("{width}x{height}"))
        } else {
            None
        },
        fps: round1(rates.video_fps),
        expected_fps: round1(expected_fps),
        kf_fps: round1(rates.keyframe_fps),
        kbps: rates.video_kbps.round(),
        max_frame_kb: round1(snap.video_max_frame as f64 / 1024.0),
        gap_min_ms: rates.gap_min_ms.round(),
        gap_avg_ms: rates.gap_avg_ms.round(),
        gap_max_ms: rates.gap_max_ms.round(),
        jitter_samples: snap.jitter_samples,
        jitter_p50_ms: round1(snap.jitter_p50_ms),
        jitter_p99_ms: round1(snap.jitter_p99_ms),
        frames: nonzero_u64(snap.video_frames),
        bytes: nonzero_u64(snap.video_bytes),
        keyframes: nonzero_u64(snap.video_keyframes),
        reconnects: nonzero_u64(snap.reconnects),
        drops: nonzero_u64(snap.dropped_frames),
        errors: nonzero_u64(snap.error_count),
    }
}

pub(crate) fn audio_report(
    snap: &IngressSnapshot,
    rates: &IngressRates,
    codec: Option<&str>,
) -> Option<StreamReport> {
    if snap.audio_frames == 0 {
        return None;
    }
    Some(StreamReport {
        kind: "audio".into(),
        codec: codec.map(Into::into),
        resolution: None,
        fps: round1(rates.audio_fps),
        expected_fps: 0.0,
        kf_fps: 0.0,
        kbps: rates.audio_kbps.round(),
        max_frame_kb: round1(snap.audio_max_frame as f64 / 1024.0),
        gap_min_ms: 0.0,
        gap_avg_ms: 0.0,
        gap_max_ms: 0.0,
        jitter_samples: 0,
        jitter_p50_ms: 0.0,
        jitter_p99_ms: 0.0,
        frames: nonzero_u64(snap.audio_frames),
        bytes: nonzero_u64(snap.audio_bytes),
        keyframes: None,
        reconnects: None,
        drops: None,
        errors: None,
    })
}

pub(crate) fn log_camera_report(report: &CameraReport) {
    if let Ok(json) = serde_json::to_string(report) {
        tracing::info!("{json}");
    }
}

fn is_zero_f64(v: &f64) -> bool {
    *v == 0.0
}

const fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

const fn is_none_or_zero_u64(v: &Option<u64>) -> bool {
    matches!(v, None | Some(0))
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

const fn nonzero_u64(v: u64) -> Option<u64> {
    if v == 0 { None } else { Some(v) }
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_jitter_histogram_reports_quantiles_and_resets() {
        let mut jitter = FrameJitterHistogram::default();
        for gap_us in [100_000, 101_000, 102_000, 103_000, 200_000] {
            jitter.record(gap_us, 10.0);
        }

        let snapshot = jitter.snapshot_and_reset();
        assert_eq!(snapshot.samples, 5);
        assert!(snapshot.p50_us.abs_diff(2_000) <= 10);
        assert!(snapshot.p99_us.abs_diff(100_000) <= 100);
        assert_eq!(jitter.snapshot_and_reset(), FrameJitterSnapshot::default());
    }

    #[test]
    fn health_video_report_contains_every_ingress_counter() {
        let snapshot = IngressSnapshot {
            uptime_secs: 10.0,
            reconnects: 5,
            video_frames: 250,
            video_keyframes: 10,
            video_bytes: 10_485_760,
            video_max_frame: 819_200,
            audio_frames: 0,
            audio_bytes: 0,
            audio_max_frame: 0,
            dropped_frames: 3,
            error_count: 4,
            gap_min_ms: 39.2,
            gap_max_ms: 145.7,
            gap_avg_ms: 40.4,
            jitter_samples: 249,
            jitter_p50_ms: 1.24,
            jitter_p99_ms: 14.54,
        };
        let rates = IngressRates {
            video_fps: 24.96,
            keyframe_fps: 1.04,
            video_kbps: 8_192.4,
            audio_fps: 0.0,
            audio_kbps: 0.0,
            gap_min_ms: snapshot.gap_min_ms,
            gap_max_ms: snapshot.gap_max_ms,
            gap_avg_ms: snapshot.gap_avg_ms,
        };

        let report = video_report(
            StreamKind::Main,
            &snapshot,
            &rates,
            Some(&VideoEncoding::H265),
            3840,
            2160,
            25.0,
        );

        assert_eq!(report.kind, "video_main");
        assert_eq!(report.codec.as_deref(), Some("h265"));
        assert_eq!(report.resolution.as_deref(), Some("3840x2160"));
        assert_eq!(report.fps, 25.0);
        assert_eq!(report.expected_fps, 25.0);
        assert_eq!(report.kf_fps, 1.0);
        assert_eq!(report.kbps, 8_192.0);
        assert_eq!(report.max_frame_kb, 800.0);
        assert_eq!(report.gap_min_ms, 39.0);
        assert_eq!(report.gap_avg_ms, 40.0);
        assert_eq!(report.gap_max_ms, 146.0);
        assert_eq!(report.jitter_samples, 249);
        assert_eq!(report.jitter_p50_ms, 1.2);
        assert_eq!(report.jitter_p99_ms, 14.5);
        assert_eq!(report.frames, Some(250));
        assert_eq!(report.bytes, Some(10_485_760));
        assert_eq!(report.keyframes, Some(10));
        assert_eq!(report.reconnects, Some(5));
        assert_eq!(report.drops, Some(3));
        assert_eq!(report.errors, Some(4));
    }

    fn stream(kind: &str, fps: f64) -> StreamReport {
        StreamReport {
            kind: kind.to_owned(),
            codec: Some("h264".to_owned()),
            resolution: Some("640x360".to_owned()),
            fps,
            expected_fps: 15.0,
            kf_fps: 1.0,
            kbps: 512.0,
            max_frame_kb: 24.0,
            gap_min_ms: 60.0,
            gap_avg_ms: 66.0,
            gap_max_ms: 80.0,
            jitter_samples: 99,
            jitter_p50_ms: 2.0,
            jitter_p99_ms: 14.0,
            frames: Some(100),
            bytes: Some(1_000),
            keyframes: Some(10),
            reconnects: Some(1),
            drops: None,
            errors: None,
        }
    }

    #[test]
    fn registry_merges_streams_and_replaces_only_matching_kind() {
        let registry = HealthRegistry::new();
        let ip = IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 1));
        registry.publish(CameraReport {
            ip,
            name: Some("camera".to_owned()),
            brand: Some("brand".to_owned()),
            port: 554,
            streams: vec![stream("video_main", 25.0)],
        });
        registry.publish(CameraReport {
            ip,
            name: None,
            brand: None,
            port: 554,
            streams: vec![stream("video_sub", 15.0)],
        });
        registry.publish(CameraReport {
            ip,
            name: None,
            brand: None,
            port: 554,
            streams: vec![stream("video_main", 24.0)],
        });

        let reports = registry.snapshot();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].name.as_deref(), Some("camera"));
        assert_eq!(reports[0].streams.len(), 2);
        assert_eq!(reports[0].streams[0].report.kind, "video_main");
        assert_eq!(reports[0].streams[0].report.fps, 24.0);
        assert_eq!(reports[0].streams[1].report.kind, "video_sub");
        assert_eq!(reports[0].streams[1].report.fps, 15.0);
    }

    #[test]
    fn registry_tracks_media_progress_with_monotonic_ages_and_recent_deltas() {
        let registry = HealthRegistry::new();
        let ip = IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 2));
        let started_at = Instant::now();
        let mut initial = stream("video_main", 15.0);
        initial.drops = Some(3);
        initial.errors = Some(4);
        registry.publish_at(
            CameraReport {
                ip,
                name: Some("camera".to_owned()),
                brand: None,
                port: 554,
                streams: vec![initial.clone()],
            },
            started_at,
            1_000,
        );

        let mut next = initial;
        next.frames = Some(120);
        next.errors = Some(5);
        registry.publish_at(
            CameraReport {
                ip,
                name: None,
                brand: None,
                port: 554,
                streams: vec![next],
            },
            started_at + Duration::from_secs(10),
            11_000,
        );

        let reports = registry.snapshot_at(started_at + Duration::from_secs(35));
        let report = &reports[0].streams[0];
        assert_eq!(report.frame_updated_at_ms, Some(11_000));
        assert_eq!(report.frame_age_ms, Some(25_000));
        assert_eq!(report.keyframe_updated_at_ms, Some(1_000));
        assert_eq!(report.keyframe_age_ms, Some(35_000));
        assert_eq!(report.recent_drops, 0);
        assert_eq!(report.recent_errors, 1);

        let mut reset = stream("video_main", 15.0);
        reset.frames = Some(5);
        reset.keyframes = Some(1);
        reset.reconnects = Some(1);
        reset.drops = Some(1);
        reset.errors = None;
        registry.publish_at(
            CameraReport {
                ip,
                name: None,
                brand: None,
                port: 554,
                streams: vec![reset],
            },
            started_at + Duration::from_secs(40),
            41_000,
        );

        let reports = registry.snapshot_at(started_at + Duration::from_secs(40));
        let report = &reports[0].streams[0];
        assert_eq!(report.frame_age_ms, Some(0));
        assert_eq!(report.keyframe_age_ms, Some(0));
        assert_eq!(report.recent_drops, 1);
        assert_eq!(report.recent_errors, 0);
    }

    #[test]
    fn audio_report_includes_frame_rate_and_maximum_payload_size() {
        let mut stats = IngressStats::new();
        let mut previous = stats.snapshot();
        stats.on_audio_frame(1_600);
        stats.on_audio_frame(512);
        let mut snapshot = stats.snapshot();
        previous.uptime_secs = 0.0;
        snapshot.uptime_secs = 1.0;
        let rates = snapshot.rates_since(&previous);

        let report = audio_report(&snapshot, &rates, Some("aac")).unwrap();

        assert_eq!(report.fps, 2.0);
        assert_eq!(report.max_frame_kb, 1.6);
        assert_eq!(report.frames, Some(2));
        assert_eq!(report.keyframes, None);
    }
}
