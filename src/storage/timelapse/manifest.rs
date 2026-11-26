//! Versioned timelapse provenance manifest serialization and hashing.

use super::{
    MANIFEST_SCHEMA_VERSION, TimelapseCadence, TimelapseGap, TimelapseGapReason, TimelapsePlan,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelapseManifestSampling {
    pub cadence: TimelapseCadence,
    pub tolerance_ms: i64,
    pub deduplicated_targets: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelapseManifestGeneration {
    pub output_fps: u32,
    pub output_width: u32,
    pub output_height: u32,
    pub timestamp_overlay: bool,
    pub silent: bool,
    pub allow_partial: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelapseManifestSample {
    pub output_frame_index: u32,
    pub output_pts_ms: u64,
    pub requested_time_ms: i64,
    pub actual_time_ms: i64,
    pub actual_time_ns: i128,
    pub source_ticks: Option<i64>,
    pub source_timescale: Option<u32>,
    pub recording_id: String,
    pub fragment_sequence: u64,
    pub sample_ordinal: u32,
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub fragment_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelapseManifestGap {
    pub start_ms: i64,
    pub end_ms: i64,
    pub reason: TimelapseGapReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelapseManifest {
    pub schema_version: u32,
    pub derivative_notice: String,
    pub source_id: String,
    pub stream_id: String,
    pub source_identity: Option<String>,
    pub source_fingerprint: String,
    pub catalog_revision: u64,
    pub requested_start_ms: i64,
    pub requested_end_ms: i64,
    pub site_timezone: String,
    pub utc_provenance: bool,
    pub sampling: TimelapseManifestSampling,
    pub generation: TimelapseManifestGeneration,
    pub samples: Vec<TimelapseManifestSample>,
    pub gaps: Vec<TimelapseManifestGap>,
    pub output_file_name: String,
    pub output_sha256: String,
    pub output_bytes: u64,
    pub output_duration_ms: u64,
}

pub struct ManifestOutputMetadata<'a> {
    pub source_identity: Option<String>,
    pub output_file_name: &'a str,
    pub output_sha256: &'a str,
    pub output_bytes: u64,
    pub output_width: u32,
    pub output_height: u32,
    pub output_fps: u32,
    pub timestamp_overlay: bool,
    pub allow_partial: bool,
}

impl TimelapseManifest {
    /// Builds a deterministic provenance manifest from a completed timelapse plan and artifact.
    pub fn from_plan(plan: &TimelapsePlan, output: ManifestOutputMetadata<'_>) -> Self {
        let fps = u64::from(output.output_fps.max(1));
        let (samples, gaps) = collect_manifest_items(&plan.items, fps);
        let output_duration_ms = (samples.len() as u64).saturating_mul(1_000) / fps;
        Self {
            schema_version: MANIFEST_SCHEMA_VERSION,
            derivative_notice:
                "Time-compressed silent derivative generated from sampled source frames; not continuous source evidence."
                    .to_owned(),
            source_id: plan.source_id.clone(),
            stream_id: plan.stream_id.clone(),
            source_identity: output.source_identity,
            source_fingerprint: plan.source_fingerprint.clone(),
            catalog_revision: plan.catalog_revision,
            requested_start_ms: plan.requested_start_ms,
            requested_end_ms: plan.requested_end_ms,
            site_timezone: plan.site_timezone.clone(),
            utc_provenance: true,
            sampling: TimelapseManifestSampling {
                cadence: plan.cadence,
                tolerance_ms: plan.tolerance_ms,
                deduplicated_targets: plan.deduplicated_targets,
            },
            generation: TimelapseManifestGeneration {
                output_fps: output.output_fps,
                output_width: output.output_width,
                output_height: output.output_height,
                timestamp_overlay: output.timestamp_overlay,
                silent: true,
                allow_partial: output.allow_partial,
            },
            samples,
            gaps,
            output_file_name: output.output_file_name.to_owned(),
            output_sha256: output.output_sha256.to_owned(),
            output_bytes: output.output_bytes,
            output_duration_ms,
        }
    }

    /// Serializes the manifest as deterministic UTF-8 JSON and returns `(bytes, sha256_hex)`.
    pub fn to_json_and_sha256(&self) -> anyhow::Result<(Vec<u8>, String)> {
        let bytes = serde_json::to_vec_pretty(self)?;
        let sha256 = encode_lower_hex(&Sha256::digest(&bytes));
        Ok((bytes, sha256))
    }
}

fn collect_manifest_items(
    items: &[super::TimelapsePlanItem],
    fps: u64,
) -> (Vec<TimelapseManifestSample>, Vec<TimelapseManifestGap>) {
    let mut samples = Vec::new();
    let mut gaps = Vec::new();
    for item in items {
        match item {
            super::TimelapsePlanItem::Sample(sample) => {
                let frame_index = u32::try_from(samples.len()).unwrap_or(u32::MAX);
                let output_pts_ms = (u64::from(frame_index) * 1_000) / fps;
                samples.push(TimelapseManifestSample {
                    output_frame_index: frame_index,
                    output_pts_ms,
                    requested_time_ms: sample.requested_time_ms,
                    actual_time_ms: sample.actual_time_ms,
                    actual_time_ns: sample.actual_time_ns,
                    source_ticks: sample.source_ticks,
                    source_timescale: sample.source_timescale,
                    recording_id: sample.recording_id.clone(),
                    fragment_sequence: sample.fragment_sequence,
                    sample_ordinal: sample.sample_ordinal,
                    codec: sample.codec.clone(),
                    width: sample.width,
                    height: sample.height,
                    fragment_fingerprint: encode_lower_hex(&sample.fragment_fingerprint),
                });
            }
            super::TimelapsePlanItem::Gap(TimelapseGap {
                start_ms,
                end_ms,
                reason,
            }) => {
                gaps.push(TimelapseManifestGap {
                    start_ms: *start_ms,
                    end_ms: *end_ms,
                    reason: *reason,
                });
            }
        }
    }
    (samples, gaps)
}

pub(super) fn encode_lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(char::from(HEX[(byte >> 4) as usize]));
        output.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    output
}
