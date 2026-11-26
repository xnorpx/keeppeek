//! Bounded sparse timelapse planner, gap classification, and estimate oracle.

pub mod manifest;
pub mod media;

pub use manifest::{ManifestOutputMetadata, TimelapseManifest};
pub use media::{
    DecodeWindowSample, SampleDecodeWindow, SampleTokenPayload, decode_sample_token,
    encode_sample_token, read_sample_decode_window,
};

use crate::storage::{CatalogMediaFragment, CatalogTimelapseFragment, RecordingCatalogHandle};
use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

pub const MAX_RANGE_MS: i64 = 31 * 86_400_000;
pub const MAX_SAMPLES: u32 = 3_600;
pub const MIN_OUTPUT_WIDTH: u32 = 64;
pub const MIN_OUTPUT_HEIGHT: u32 = 64;
pub const MAX_OUTPUT_WIDTH: u32 = 1_920;
pub const MAX_OUTPUT_HEIGHT: u32 = 1_080;
pub const MIN_OUTPUT_FPS: u32 = 1;
pub const MAX_OUTPUT_FPS: u32 = 30;
pub const MAX_OUTPUT_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_SOURCE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const DEFAULT_TOLERANCE_MS: i64 = 10_000;
pub const MAX_TOLERANCE_MS: i64 = 300_000;
pub const MAX_CONCURRENT_JOBS_GLOBAL: usize = 1;
pub const MAX_CONCURRENT_JOBS_PER_REQUESTER: usize = 1;
pub const MAX_CONCURRENT_VISIBLE_THUMBNAILS: usize = 2;
pub const MAX_THUMBNAIL_CACHE_BYTES: u64 = 64 * 1024 * 1024;
pub const DEFAULT_PAGE_SIZE: u32 = 32;
pub const MAX_PAGE_SIZE: u32 = 128;
pub const EXPENSIVE_SAMPLE_THRESHOLD: u32 = 600;
pub const EXPENSIVE_OUTPUT_BYTES_THRESHOLD: u64 = 64 * 1024 * 1024;
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelapsePreset {
    Last15Minutes,
    LastHour,
    Custom,
}

impl TimelapsePreset {
    /// Resolves a preset window in absolute UTC milliseconds using checked integer math.
    pub fn resolve_range_ms(
        self,
        now_ms: i64,
        custom: Option<(i64, i64)>,
    ) -> anyhow::Result<(i64, i64)> {
        match self {
            Self::Last15Minutes => {
                let start = now_ms
                    .checked_sub(15 * 60 * 1_000)
                    .context("last-15-minutes start timestamp underflow")?;
                Ok((start, now_ms))
            }
            Self::LastHour => {
                let start = now_ms
                    .checked_sub(60 * 60 * 1_000)
                    .context("last-hour start timestamp underflow")?;
                Ok((start, now_ms))
            }
            Self::Custom => {
                let (start, end) = custom.context("custom preset requires explicit range")?;
                validate_range_ms(start, end)?;
                Ok((start, end))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "mode", content = "value", rename_all = "snake_case")]
pub enum TimelapseCadence {
    IntervalMs(u64),
    TargetCount(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelapseGapReason {
    Missing,
    Retained,
    Privacy,
    UnsupportedCodec,
    Corrupt,
    OpenRecording,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TimelapseOutputPreview {
    pub output_fps: u32,
    pub max_width: u32,
    pub max_height: u32,
    pub timestamp_overlay: bool,
}

impl Default for TimelapseOutputPreview {
    fn default() -> Self {
        Self {
            output_fps: 10,
            max_width: 1_280,
            max_height: 720,
            timestamp_overlay: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TimelapsePlanRequest {
    pub source_id: String,
    pub stream_id: String,
    pub catalog_stream_id: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub cadence: TimelapseCadence,
    pub tolerance_ms: i64,
    pub include_open_recordings: bool,
    pub privacy_intervals: Vec<(i64, i64)>,
    pub output_preview: TimelapseOutputPreview,
    pub site_timezone: String,
    pub encoder_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelapseSample {
    pub index: u32,
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
    pub random_access: bool,
    pub encoded_bytes: u32,
    pub fragment_fingerprint: [u8; 32],
    pub sample_token: Vec<u8>,
    pub fragment: CatalogMediaFragment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelapseGap {
    pub start_ms: i64,
    pub end_ms: i64,
    pub reason: TimelapseGapReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelapsePlanItem {
    Sample(Box<TimelapseSample>),
    Gap(TimelapseGap),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelapseEstimate {
    pub sample_count: u32,
    pub gap_count: u32,
    pub gap_duration_ms: u64,
    pub output_duration_ms: u64,
    pub estimated_source_bytes: u64,
    pub estimated_output_bytes: u64,
    pub requires_confirmation: bool,
    pub encoder_available: bool,
}

#[derive(Debug, Clone)]
pub struct TimelapsePlan {
    pub source_id: String,
    pub stream_id: String,
    pub requested_start_ms: i64,
    pub requested_end_ms: i64,
    pub cadence: TimelapseCadence,
    pub tolerance_ms: i64,
    pub catalog_revision: u64,
    pub source_fingerprint: String,
    pub site_timezone: String,
    pub deduplicated_targets: u32,
    pub estimate: TimelapseEstimate,
    pub items: Vec<TimelapsePlanItem>,
}

#[derive(Debug, Clone)]
pub struct TimelapsePlanPage {
    pub items: Vec<TimelapsePlanItem>,
    pub next_offset: Option<usize>,
}

impl TimelapsePlan {
    /// Returns true if any requested interval could not be resolved to a valid sample.
    pub const fn has_gaps(&self) -> bool {
        self.estimate.gap_count > 0
    }

    /// Returns a bounded page slice of plan items for metadata-first interactive review.
    pub fn page(&self, offset: usize, page_size: u32) -> anyhow::Result<TimelapsePlanPage> {
        let size = page_size.clamp(1, MAX_PAGE_SIZE) as usize;
        if offset >= self.items.len() {
            return Ok(TimelapsePlanPage {
                items: Vec::new(),
                next_offset: None,
            });
        }
        let end = offset.saturating_add(size).min(self.items.len());
        let next_offset = (end < self.items.len()).then_some(end);
        Ok(TimelapsePlanPage {
            items: self.items[offset..end].to_vec(),
            next_offset,
        })
    }

    /// Returns an iterator over resolved samples in monotonic order.
    pub fn samples(&self) -> impl Iterator<Item = &TimelapseSample> {
        self.items.iter().filter_map(|item| match item {
            TimelapsePlanItem::Sample(sample) => Some(sample.as_ref()),
            TimelapsePlanItem::Gap(_) => None,
        })
    }
}

pub fn validate_range_ms(start_ms: i64, end_ms: i64) -> anyhow::Result<i64> {
    let span = end_ms
        .checked_sub(start_ms)
        .context("timelapse range duration overflow")?;
    ensure!(span > 0, "timelapse end time must follow start time");
    ensure!(
        span <= MAX_RANGE_MS,
        "timelapse range exceeds maximum of 31 days"
    );
    Ok(span)
}

pub fn validate_output_settings(preview: &TimelapseOutputPreview) -> anyhow::Result<()> {
    ensure!(
        (MIN_OUTPUT_FPS..=MAX_OUTPUT_FPS).contains(&preview.output_fps),
        "timelapse output FPS must be between {MIN_OUTPUT_FPS} and {MAX_OUTPUT_FPS}"
    );
    ensure!(
        (MIN_OUTPUT_WIDTH..=MAX_OUTPUT_WIDTH).contains(&preview.max_width)
            && preview.max_width.is_multiple_of(2),
        "timelapse output width must be even and between {MIN_OUTPUT_WIDTH} and {MAX_OUTPUT_WIDTH}"
    );
    ensure!(
        (MIN_OUTPUT_HEIGHT..=MAX_OUTPUT_HEIGHT).contains(&preview.max_height)
            && preview.max_height.is_multiple_of(2),
        "timelapse output height must be even and between {MIN_OUTPUT_HEIGHT} and {MAX_OUTPUT_HEIGHT}"
    );
    Ok(())
}

/// Generates strictly monotonic UTC target timestamps with checked integer arithmetic.
pub fn generate_target_times(
    start_ms: i64,
    end_ms: i64,
    cadence: TimelapseCadence,
) -> anyhow::Result<Vec<i64>> {
    let span = validate_range_ms(start_ms, end_ms)?;
    match cadence {
        TimelapseCadence::IntervalMs(interval_ms) => {
            ensure!(interval_ms > 0, "timelapse interval must be positive");
            let interval = i64::try_from(interval_ms).context("timelapse interval overflow")?;
            let count_minus_one = (span - 1) / interval;
            let count = count_minus_one
                .checked_add(1)
                .context("timelapse sample count overflow")?;
            ensure!(
                count <= i64::from(MAX_SAMPLES),
                "timelapse sample count {count} exceeds maximum {MAX_SAMPLES}"
            );
            let mut targets = Vec::with_capacity(count as usize);
            for step in 0..count {
                let offset = step
                    .checked_mul(interval)
                    .context("timelapse target offset overflow")?;
                let ts = start_ms
                    .checked_add(offset)
                    .context("timelapse target timestamp overflow")?;
                targets.push(ts);
            }
            Ok(targets)
        }
        TimelapseCadence::TargetCount(count) => {
            ensure!(
                (1..=MAX_SAMPLES).contains(&count),
                "timelapse target count must be between 1 and {MAX_SAMPLES}"
            );
            let mut targets = Vec::with_capacity(count as usize);
            let span_u = u128::try_from(span).context("timelapse span conversion")?;
            let count_u = u128::from(count);
            for index in 0..count {
                let offset_u = span_u
                    .checked_mul(u128::from(index))
                    .context("timelapse target multiplication overflow")?
                    / count_u;
                let offset = i64::try_from(offset_u).context("timelapse target offset overflow")?;
                let ts = start_ms
                    .checked_add(offset)
                    .context("timelapse target timestamp overflow")?;
                if targets.last().copied().is_none_or(|prev| ts > prev) {
                    targets.push(ts);
                }
            }
            Ok(targets)
        }
    }
}

/// Plans a bounded sparse timelapse from catalog metadata and bounded fragment headers only.
pub fn plan_timelapse(
    catalog: &RecordingCatalogHandle,
    request: &TimelapsePlanRequest,
    deadline: Instant,
    cancel: &AtomicBool,
) -> anyhow::Result<TimelapsePlan> {
    validate_range_ms(request.start_ms, request.end_ms)?;
    validate_output_settings(&request.output_preview)?;
    ensure!(
        (0..=MAX_TOLERANCE_MS).contains(&request.tolerance_ms),
        "timelapse tolerance must be between 0 and {MAX_TOLERANCE_MS} ms"
    );
    let _: chrono_tz::Tz = request
        .site_timezone
        .parse()
        .with_context(|| format!("invalid site timezone {:?}", request.site_timezone))?;
    let targets = generate_target_times(request.start_ms, request.end_ms, request.cadence)?;
    ensure!(
        !cancel.load(Ordering::Acquire),
        "timelapse planning cancelled"
    );
    let lookup = catalog.timelapse_lookup(
        &request.catalog_stream_id,
        request.start_ms,
        request.end_ms,
        &targets,
        request.tolerance_ms,
    )?;
    let mut state = PlanBuilder::new(request, &lookup.retained_ranges);
    for (idx, (&target_ms, candidate)) in targets.iter().zip(lookup.candidates.iter()).enumerate() {
        ensure!(
            !cancel.load(Ordering::Acquire),
            "timelapse planning cancelled"
        );
        ensure!(
            Instant::now() < deadline,
            "timelapse planning deadline exceeded"
        );
        let slot_end_ms = targets.get(idx + 1).copied().unwrap_or(request.end_ms);
        state.resolve_target(target_ms, slot_end_ms, candidate.as_ref(), deadline, cancel);
    }
    state.finish(lookup.catalog_revision)
}

struct PlanBuilder<'a> {
    request: &'a TimelapsePlanRequest,
    retained_ranges: &'a [(i64, i64)],
    cache: BTreeMap<(String, u64), Result<media::IndexedFragment, TimelapseGapReason>>,
    items: Vec<TimelapsePlanItem>,
    last_selected_key: Option<(String, u64, u32)>,
    last_actual_ns: Option<i128>,
    sample_count: u32,
    deduplicated_targets: u32,
    estimated_source_bytes: u64,
}

impl<'a> PlanBuilder<'a> {
    const fn new(request: &'a TimelapsePlanRequest, retained_ranges: &'a [(i64, i64)]) -> Self {
        Self {
            request,
            retained_ranges,
            cache: BTreeMap::new(),
            items: Vec::new(),
            last_selected_key: None,
            last_actual_ns: None,
            sample_count: 0,
            deduplicated_targets: 0,
            estimated_source_bytes: 0,
        }
    }

    fn resolve_target(
        &mut self,
        target_ms: i64,
        slot_end_ms: i64,
        candidate: Option<&CatalogTimelapseFragment>,
        deadline: Instant,
        cancel: &AtomicBool,
    ) {
        if overlaps_any(&self.request.privacy_intervals, target_ms, slot_end_ms) {
            self.push_gap(target_ms, slot_end_ms, TimelapseGapReason::Privacy);
            return;
        }
        let Some(candidate) = candidate else {
            let reason = if overlaps_any(self.retained_ranges, target_ms, slot_end_ms) {
                TimelapseGapReason::Retained
            } else {
                TimelapseGapReason::Missing
            };
            self.push_gap(target_ms, slot_end_ms, reason);
            return;
        };
        if !candidate.finalized && !self.request.include_open_recordings {
            self.push_gap(target_ms, slot_end_ms, TimelapseGapReason::OpenRecording);
            return;
        }
        let (picture, codec, width, height, fragment_fingerprint) =
            match self.select_fragment_picture(candidate, target_ms, deadline, cancel) {
                Ok(selected) => selected,
                Err(reason) => {
                    self.push_gap(target_ms, slot_end_ms, reason);
                    return;
                }
            };
        let actual_ns = media::picture_time_ns(&picture);
        let sample_key = (
            candidate.fragment.recording_id.clone(),
            candidate.fragment.sequence,
            picture.ordinal,
        );
        if self.last_selected_key.as_ref() == Some(&sample_key)
            || self.last_actual_ns.is_some_and(|prev| actual_ns <= prev)
        {
            self.deduplicated_targets = self.deduplicated_targets.saturating_add(1);
            return;
        }
        let sample_index = self.sample_count;
        self.sample_count = self.sample_count.saturating_add(1);
        self.last_selected_key = Some(sample_key);
        self.last_actual_ns = Some(actual_ns);
        self.estimated_source_bytes = self
            .estimated_source_bytes
            .saturating_add(candidate.fragment.byte_len);
        self.items
            .push(TimelapsePlanItem::Sample(Box::new(build_planned_sample(
                sample_index,
                target_ms,
                actual_ns,
                &picture,
                candidate,
                codec,
                width,
                height,
                fragment_fingerprint,
            ))));
    }

    fn select_fragment_picture(
        &mut self,
        candidate: &CatalogTimelapseFragment,
        target_ms: i64,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<
        (
            crate::storage::sample_index::Picture,
            String,
            u32,
            u32,
            [u8; 32],
        ),
        TimelapseGapReason,
    > {
        let key = (
            candidate.fragment.recording_id.clone(),
            candidate.fragment.sequence,
        );
        let entry = self.cache.entry(key).or_insert_with(|| {
            media::index_fragment_metadata(&candidate.fragment, deadline, cancel)
        });
        let indexed = match entry {
            Ok(indexed) => indexed,
            Err(reason) => return Err(*reason),
        };
        let picture =
            media::select_picture_in_fragment(indexed, target_ms, self.request.tolerance_ms)
                .ok_or(TimelapseGapReason::Missing)?;
        Ok((
            picture.clone(),
            indexed.codec.clone(),
            indexed.width,
            indexed.height,
            indexed.fragment.fingerprint,
        ))
    }

    fn push_gap(&mut self, start_ms: i64, end_ms: i64, reason: TimelapseGapReason) {
        if end_ms <= start_ms {
            return;
        }
        if let Some(TimelapsePlanItem::Gap(last)) = self.items.last_mut()
            && last.reason == reason
            && last.end_ms == start_ms
        {
            last.end_ms = end_ms;
            return;
        }
        self.items.push(TimelapsePlanItem::Gap(TimelapseGap {
            start_ms,
            end_ms,
            reason,
        }));
    }

    fn finish(mut self, catalog_revision: u64) -> anyhow::Result<TimelapsePlan> {
        let source_fingerprint = compute_source_fingerprint(
            &self.request.source_id,
            &self.request.stream_id,
            catalog_revision,
            &self.items,
        );
        for item in &mut self.items {
            if let TimelapsePlanItem::Sample(sample) = item {
                let token = encode_sample_token(&SampleTokenPayload {
                    version: 1,
                    source_id: self.request.source_id.clone(),
                    stream_id: self.request.stream_id.clone(),
                    recording_id: sample.recording_id.clone(),
                    fragment_sequence: sample.fragment_sequence,
                    sample_ordinal: sample.sample_ordinal,
                    actual_time_ns: sample.actual_time_ns,
                    fragment_fingerprint: sample.fragment_fingerprint,
                    source_fingerprint: source_fingerprint.clone(),
                })?;
                sample.sample_token = token;
            }
        }
        let estimate = build_estimate(
            self.sample_count,
            &self.items,
            self.estimated_source_bytes,
            &self.request.output_preview,
            self.request.encoder_available,
        );
        Ok(TimelapsePlan {
            source_id: self.request.source_id.clone(),
            stream_id: self.request.stream_id.clone(),
            requested_start_ms: self.request.start_ms,
            requested_end_ms: self.request.end_ms,
            cadence: self.request.cadence,
            tolerance_ms: self.request.tolerance_ms,
            catalog_revision,
            source_fingerprint,
            site_timezone: self.request.site_timezone.clone(),
            deduplicated_targets: self.deduplicated_targets,
            estimate,
            items: self.items,
        })
    }
}

fn overlaps_any(ranges: &[(i64, i64)], start_ms: i64, end_ms: i64) -> bool {
    ranges
        .iter()
        .any(|&(r_start, r_end)| r_start < end_ms && r_end > start_ms)
}

fn compute_source_fingerprint(
    source_id: &str,
    stream_id: &str,
    catalog_revision: u64,
    items: &[TimelapsePlanItem],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"keeppeek.timelapse.plan.v1\0");
    hasher.update(source_id.as_bytes());
    hasher.update([0]);
    hasher.update(stream_id.as_bytes());
    hasher.update(catalog_revision.to_le_bytes());
    for item in items {
        match item {
            TimelapsePlanItem::Sample(sample) => {
                hasher.update([1]);
                hasher.update(sample.recording_id.as_bytes());
                hasher.update(sample.fragment_sequence.to_le_bytes());
                hasher.update(sample.sample_ordinal.to_le_bytes());
                hasher.update(sample.fragment_fingerprint);
            }
            TimelapsePlanItem::Gap(gap) => {
                hasher.update([2]);
                hasher.update(gap.start_ms.to_le_bytes());
                hasher.update(gap.end_ms.to_le_bytes());
                hasher.update([gap.reason as u8]);
            }
        }
    }
    manifest::encode_lower_hex(&hasher.finalize())
}

fn build_estimate(
    sample_count: u32,
    items: &[TimelapsePlanItem],
    estimated_source_bytes: u64,
    preview: &TimelapseOutputPreview,
    encoder_available: bool,
) -> TimelapseEstimate {
    let mut gap_count = 0_u32;
    let mut gap_duration_ms = 0_u64;
    for item in items {
        if let TimelapsePlanItem::Gap(gap) = item {
            gap_count = gap_count.saturating_add(1);
            gap_duration_ms = gap_duration_ms.saturating_add(
                u64::try_from(gap.end_ms.saturating_sub(gap.start_ms)).unwrap_or(0),
            );
        }
    }
    let fps = u64::from(preview.output_fps.max(1));
    let output_duration_ms = u64::from(sample_count).saturating_mul(1_000) / fps;
    // Conservative H.264 estimate (~0.08 bytes/pixel per sampled frame) bounded to output cap.
    let pixels = u64::from(preview.max_width).saturating_mul(u64::from(preview.max_height));
    let bytes_per_frame = (pixels / 12).max(16 * 1_024);
    let estimated_output_bytes = u64::from(sample_count)
        .saturating_mul(bytes_per_frame)
        .saturating_add(64 * 1_024);
    let requires_confirmation = sample_count >= EXPENSIVE_SAMPLE_THRESHOLD
        || estimated_output_bytes >= EXPENSIVE_OUTPUT_BYTES_THRESHOLD
        || estimated_source_bytes >= MAX_SOURCE_BYTES / 4;
    TimelapseEstimate {
        sample_count,
        gap_count,
        gap_duration_ms,
        output_duration_ms,
        estimated_source_bytes,
        estimated_output_bytes,
        requires_confirmation,
        encoder_available,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_planned_sample(
    index: u32,
    requested_time_ms: i64,
    actual_time_ns: i128,
    picture: &crate::storage::sample_index::Picture,
    candidate: &CatalogTimelapseFragment,
    codec: String,
    width: u32,
    height: u32,
    fragment_fingerprint: [u8; 32],
) -> TimelapseSample {
    TimelapseSample {
        index,
        requested_time_ms,
        actual_time_ms: (actual_time_ns / 1_000_000) as i64,
        actual_time_ns,
        source_ticks: picture.source.map(|clock| clock.timestamp),
        source_timescale: picture.source.map(|clock| clock.clock_rate.get()),
        recording_id: candidate.fragment.recording_id.clone(),
        fragment_sequence: candidate.fragment.sequence,
        sample_ordinal: picture.ordinal,
        codec,
        width,
        height,
        random_access: picture.sync,
        encoded_bytes: picture.size,
        fragment_fingerprint,
        sample_token: Vec::new(),
        fragment: candidate.fragment.clone(),
    }
}
