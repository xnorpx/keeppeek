//! Non-configurable safety limits, GCRA pacing, exact rolling windows, and retry math.
//!
//! User rule cooldowns may be stricter than these bounds, but cannot raise them.
//! All clock inputs are clamped against a monotonic floor so clock rollback cannot
//! replenish capacity or extend intent lifetimes.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// Minimum outbound interval between distinct production event intents on one NVR.
pub const NVR_SOURCE_SPACING_MS: u64 = 30_000;

/// Advisory NVR GCRA emission interval for 2 attempts per second.
pub const NVR_GCRA_EMISSION_INTERVAL_MS: u64 = 500;

/// Advisory NVR GCRA burst allowance (4 attempts immediately, then 1 per 500 ms).
pub const NVR_GCRA_BURST: u32 = 4;

/// GCRA burst tolerance `tau = (burst - 1) * emission_interval`.
pub const NVR_GCRA_TOLERANCE_MS: u64 = (NVR_GCRA_BURST as u64 - 1) * NVR_GCRA_EMISSION_INTERVAL_MS;

/// Exact rolling hourly production cloud event ceiling per NVR.
pub const NVR_HOURLY_LIMIT: usize = 120;

/// Exact rolling daily production cloud event ceiling per NVR.
pub const NVR_DAILY_LIMIT: usize = 500;

/// Exact rolling hourly test signal ceiling per NVR (separate from production).
pub const NVR_TEST_HOURLY_LIMIT: usize = 3;

/// Rolling one-hour window length in milliseconds.
pub const ROLLING_HOUR_MS: i64 = 3_600_000;

/// Rolling one-day window length in milliseconds.
pub const ROLLING_DAY_MS: i64 = 86_400_000;

/// Maximum outbound attempts per logical signal intent (including ambiguous handoffs).
pub const MAX_ATTEMPTS_PER_INTENT: u32 = 3;

/// Maximum lifetime of one outbound signal intent in milliseconds (5 minutes).
pub const INTENT_TTL_MS: i64 = 300_000;

/// Base exponential backoff cap for attempt 1 retry before full jitter.
pub const RETRY_BACKOFF_BASE_MS: u64 = 5_000;

/// Upper cap for jittered retry delay when no `Retry-After` header applies.
pub const RETRY_BACKOFF_CAP_MS: u64 = 60_000;

/// Maximum `Retry-After` duration honored before clamping (1 hour).
pub const MAX_RETRY_AFTER_MS: u64 = 3_600_000;

/// Maximum distinct suppression counter buckets retained across a rolling 24-hour window.
const MAX_SUPPRESSION_BUCKETS: usize = 1_440;

/// Coalescing granularity used once exact suppression timestamps reach `MAX_SUPPRESSION_BUCKETS`.
const SUPPRESSION_BUCKET_MS: i64 = 60_000;

const _: () = {
    assert!(NVR_HOURLY_LIMIT < NVR_DAILY_LIMIT);
    assert!(NVR_GCRA_BURST >= 1);
    assert!(NVR_GCRA_TOLERANCE_MS == 1_500);
    assert!(MAX_ATTEMPTS_PER_INTENT == 3);
    assert!(INTENT_TTL_MS == 5 * 60 * 1_000);
    assert!(MAX_SUPPRESSION_BUCKETS == (ROLLING_DAY_MS / SUPPRESSION_BUCKET_MS) as usize);
};

/// Closed failure reason codes recorded in status, test results, and local history.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudFailureCode {
    #[default]
    None,
    LocalLimit,
    ServiceLimit,
    Dependency,
    CredentialRejected,
    Integrity,
    Expired,
}

impl CloudFailureCode {
    /// Returns the bounded lowercase wire/history label for this failure code.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::LocalLimit => "cloud_local_limit",
            Self::ServiceLimit => "cloud_service_limit",
            Self::Dependency => "cloud_dependency",
            Self::CredentialRejected => "cloud_credential_rejected",
            Self::Integrity => "cloud_integrity",
            Self::Expired => "cloud_expired",
        }
    }
}

/// Bounded operational state reported in `CloudNotificationStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudState {
    Disabled,
    Unpaired,
    Ready,
    Throttled,
    BackingOff,
    Unavailable,
}

impl CloudState {
    /// Returns the bounded lowercase label used in Prometheus metrics.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Unpaired => "unpaired",
            Self::Ready => "ready",
            Self::Throttled => "throttled",
            Self::BackingOff => "backing_off",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Outcome of evaluating local NVR pacing before sending a signal attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaceDecision {
    Allowed,
    Delayed {
        next_eligible_at_ms: i64,
        reason: CloudFailureCode,
    },
}

/// Persisted bounded pacing and rolling-window state for one NVR.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalPacingState {
    pub clock_floor_ms: i64,
    pub gcra_tat_ms: i64,
    pub last_intent_sent_at_ms: Option<i64>,
    pub service_retry_after_until_ms: Option<i64>,
    pub backoff_until_ms: Option<i64>,
    pub accepted_day_ms: VecDeque<i64>,
    pub test_hour_ms: VecDeque<i64>,
    pub suppressed_buckets: VecDeque<(i64, u32)>,
    pub last_accepted_at_ms: Option<i64>,
    pub last_failure: CloudFailureCode,
    pub credential_rejected: bool,
}

impl LocalPacingState {
    /// Clamps `raw_now_ms` against the stored monotonic clock floor and advances the floor.
    pub fn clamp_now(&mut self, raw_now_ms: i64) -> i64 {
        let effective = raw_now_ms.max(self.clock_floor_ms);
        self.clock_floor_ms = effective;
        self.prune_expired(effective);
        effective
    }

    /// Computes effective time without mutating stored state.
    pub fn effective_now(&self, raw_now_ms: i64) -> i64 {
        raw_now_ms.max(self.clock_floor_ms)
    }

    /// Removes entries outside their exact rolling windows as of `now_ms`.
    pub fn prune_expired(&mut self, now_ms: i64) {
        let day_cutoff = now_ms.saturating_sub(ROLLING_DAY_MS);
        while self
            .accepted_day_ms
            .front()
            .is_some_and(|&timestamp| timestamp <= day_cutoff)
        {
            self.accepted_day_ms.pop_front();
        }
        let hour_cutoff = now_ms.saturating_sub(ROLLING_HOUR_MS);
        while self
            .test_hour_ms
            .front()
            .is_some_and(|&timestamp| timestamp <= hour_cutoff)
        {
            self.test_hour_ms.pop_front();
        }
        while self
            .suppressed_buckets
            .front()
            .is_some_and(|&(timestamp, _)| timestamp <= day_cutoff)
        {
            self.suppressed_buckets.pop_front();
        }
        if self
            .service_retry_after_until_ms
            .is_some_and(|until| until <= now_ms)
        {
            self.service_retry_after_until_ms = None;
        }
        if self.backoff_until_ms.is_some_and(|until| until <= now_ms) {
            self.backoff_until_ms = None;
        }
    }

    /// Evaluates whether a production outbound attempt is allowed right now.
    ///
    /// When `is_retry` is true, 30-second inter-intent spacing is not re-applied because the
    /// same intent is already active; GCRA, service `Retry-After`, and hourly/daily caps apply.
    pub fn check_production_attempt(&mut self, raw_now_ms: i64, is_retry: bool) -> PaceDecision {
        let now_ms = self.clamp_now(raw_now_ms);
        if let Some(until_ms) = self.service_retry_after_until_ms
            && now_ms < until_ms
        {
            return PaceDecision::Delayed {
                next_eligible_at_ms: until_ms,
                reason: CloudFailureCode::ServiceLimit,
            };
        }
        if !is_retry
            && let Some(last_sent_ms) = self.last_intent_sent_at_ms
            && let spacing_until = last_sent_ms
                .saturating_add(i64::try_from(NVR_SOURCE_SPACING_MS).unwrap_or(i64::MAX))
            && now_ms < spacing_until
        {
            return PaceDecision::Delayed {
                next_eligible_at_ms: spacing_until,
                reason: CloudFailureCode::LocalLimit,
            };
        }
        if let Some(eligible_at_ms) = self.hourly_or_daily_block_until(now_ms) {
            return PaceDecision::Delayed {
                next_eligible_at_ms: eligible_at_ms,
                reason: CloudFailureCode::LocalLimit,
            };
        }
        if let Some(eligible_at_ms) = gcra_next_eligible_ms(self.gcra_tat_ms, now_ms) {
            return PaceDecision::Delayed {
                next_eligible_at_ms: eligible_at_ms,
                reason: CloudFailureCode::LocalLimit,
            };
        }
        PaceDecision::Allowed
    }

    /// Consumes one advisory GCRA attempt token at `raw_now_ms`.
    pub fn consume_gcra_attempt(&mut self, raw_now_ms: i64) -> i64 {
        let now_ms = self.clamp_now(raw_now_ms);
        self.gcra_tat_ms = self
            .gcra_tat_ms
            .max(now_ms)
            .saturating_add(i64::try_from(NVR_GCRA_EMISSION_INTERVAL_MS).unwrap_or(i64::MAX));
        now_ms
    }

    /// Records one accepted production signal in the exact rolling 120/h and 500/d windows.
    pub fn record_production_accepted(&mut self, raw_now_ms: i64) {
        let now_ms = self.clamp_now(raw_now_ms);
        self.last_intent_sent_at_ms = Some(now_ms);
        self.last_accepted_at_ms = Some(now_ms);
        self.last_failure = CloudFailureCode::None;
        self.service_retry_after_until_ms = None;
        self.backoff_until_ms = None;
        self.accepted_day_ms.push_back(now_ms);
        while self.accepted_day_ms.len() > NVR_DAILY_LIMIT {
            self.accepted_day_ms.pop_front();
        }
    }

    /// Checks and reserves one slot in the separate 3/hour test-signal window.
    pub fn try_consume_test_signal(&mut self, raw_now_ms: i64) -> PaceDecision {
        let now_ms = self.clamp_now(raw_now_ms);
        if self.test_hour_ms.len() >= NVR_TEST_HOURLY_LIMIT {
            let oldest = *self
                .test_hour_ms
                .front()
                .expect("non-empty test window must have an oldest entry");
            return PaceDecision::Delayed {
                next_eligible_at_ms: oldest.saturating_add(ROLLING_HOUR_MS),
                reason: CloudFailureCode::LocalLimit,
            };
        }
        if let Some(eligible_at_ms) = gcra_next_eligible_ms(self.gcra_tat_ms, now_ms) {
            return PaceDecision::Delayed {
                next_eligible_at_ms: eligible_at_ms,
                reason: CloudFailureCode::LocalLimit,
            };
        }
        self.consume_gcra_attempt(now_ms);
        self.test_hour_ms.push_back(now_ms);
        while self.test_hour_ms.len() > NVR_TEST_HOURLY_LIMIT {
            self.test_hour_ms.pop_front();
        }
        PaceDecision::Allowed
    }

    /// Records `count` suppressed or replaced cloud events in bounded rolling buckets.
    pub fn record_suppression(&mut self, raw_now_ms: i64, count: u32) {
        if count == 0 {
            return;
        }
        let now_ms = self.clamp_now(raw_now_ms);
        if let Some((last_ts, last_count)) = self.suppressed_buckets.back_mut()
            && *last_ts == now_ms
        {
            *last_count = last_count.saturating_add(count);
            return;
        }
        if self.suppressed_buckets.len() >= MAX_SUPPRESSION_BUCKETS {
            self.coalesce_suppressions_to_minutes();
            let bucket_ts = now_ms - now_ms.rem_euclid(SUPPRESSION_BUCKET_MS);
            if let Some((last_ts, last_count)) = self.suppressed_buckets.back_mut()
                && *last_ts == bucket_ts
            {
                *last_count = last_count.saturating_add(count);
                return;
            }
            self.suppressed_buckets.push_back((bucket_ts, count));
            while self.suppressed_buckets.len() > MAX_SUPPRESSION_BUCKETS {
                self.suppressed_buckets.pop_front();
            }
            return;
        }
        self.suppressed_buckets.push_back((now_ms, count));
    }

    /// Returns `(accepted_last_hour, accepted_last_day)` as of `raw_now_ms`.
    pub fn accepted_counts(&self, raw_now_ms: i64) -> (u32, u32) {
        let now_ms = self.effective_now(raw_now_ms);
        let hour_cutoff = now_ms.saturating_sub(ROLLING_HOUR_MS);
        let day_cutoff = now_ms.saturating_sub(ROLLING_DAY_MS);
        let mut hour = 0_u32;
        let mut day = 0_u32;
        for &timestamp in &self.accepted_day_ms {
            if timestamp > day_cutoff {
                day = day.saturating_add(1);
                if timestamp > hour_cutoff {
                    hour = hour.saturating_add(1);
                }
            }
        }
        (hour, day)
    }

    /// Returns `(suppressed_last_hour, suppressed_last_day)` as of `raw_now_ms`.
    pub fn suppressed_counts(&self, raw_now_ms: i64) -> (u32, u32) {
        let now_ms = self.effective_now(raw_now_ms);
        let hour_cutoff = now_ms.saturating_sub(ROLLING_HOUR_MS);
        let day_cutoff = now_ms.saturating_sub(ROLLING_DAY_MS);
        let mut hour = 0_u32;
        let mut day = 0_u32;
        for &(timestamp, count) in &self.suppressed_buckets {
            if timestamp > day_cutoff {
                day = day.saturating_add(count);
                if timestamp > hour_cutoff {
                    hour = hour.saturating_add(count);
                }
            }
        }
        (hour, day)
    }

    /// Returns the earliest timestamp when a new production intent can be sent, if throttled.
    pub fn next_eligible_at_ms(&self, raw_now_ms: i64) -> Option<i64> {
        let now_ms = self.effective_now(raw_now_ms);
        let mut next: Option<i64> = None;
        let mut bump = |candidate: i64| {
            if candidate > now_ms {
                next = Some(next.map_or(candidate, |current| current.max(candidate)));
            }
        };
        if let Some(until) = self.service_retry_after_until_ms {
            bump(until);
        }
        if let Some(until) = self.backoff_until_ms {
            bump(until);
        }
        if let Some(last_sent) = self.last_intent_sent_at_ms {
            bump(
                last_sent.saturating_add(i64::try_from(NVR_SOURCE_SPACING_MS).unwrap_or(i64::MAX)),
            );
        }
        if let Some(until) = self.hourly_or_daily_block_until(now_ms) {
            bump(until);
        }
        if let Some(until) = gcra_next_eligible_ms(self.gcra_tat_ms, now_ms) {
            bump(until);
        }
        next
    }

    /// Derives the truthful operational state for status reporting.
    pub fn derive_state(&self, enabled: bool, paired: bool, raw_now_ms: i64) -> CloudState {
        if !paired {
            return CloudState::Unpaired;
        }
        if !enabled {
            return CloudState::Disabled;
        }
        if self.credential_rejected {
            return CloudState::Unavailable;
        }
        let now_ms = self.effective_now(raw_now_ms);
        if self
            .service_retry_after_until_ms
            .is_some_and(|until| until > now_ms)
            || self.hourly_or_daily_block_until(now_ms).is_some()
        {
            return CloudState::Throttled;
        }
        if self.backoff_until_ms.is_some_and(|until| until > now_ms) {
            return CloudState::BackingOff;
        }
        CloudState::Ready
    }

    fn hourly_or_daily_block_until(&self, now_ms: i64) -> Option<i64> {
        let day_cutoff = now_ms.saturating_sub(ROLLING_DAY_MS);
        let hour_cutoff = now_ms.saturating_sub(ROLLING_HOUR_MS);
        let mut day_active = 0_usize;
        let mut oldest_day = None;
        let mut hour_active = 0_usize;
        let mut oldest_hour = None;
        for &timestamp in &self.accepted_day_ms {
            if timestamp > day_cutoff {
                day_active += 1;
                if oldest_day.is_none() {
                    oldest_day = Some(timestamp);
                }
                if timestamp > hour_cutoff {
                    hour_active += 1;
                    if oldest_hour.is_none() {
                        oldest_hour = Some(timestamp);
                    }
                }
            }
        }
        let day_block = (day_active >= NVR_DAILY_LIMIT)
            .then(|| oldest_day.map(|ts| ts.saturating_add(ROLLING_DAY_MS)))
            .flatten();
        let hour_block = (hour_active >= NVR_HOURLY_LIMIT)
            .then(|| oldest_hour.map(|ts| ts.saturating_add(ROLLING_HOUR_MS)))
            .flatten();
        match (hour_block, day_block) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (Some(value), None) | (None, Some(value)) => Some(value),
            (None, None) => None,
        }
    }

    fn coalesce_suppressions_to_minutes(&mut self) {
        let mut merged: VecDeque<(i64, u32)> = VecDeque::with_capacity(MAX_SUPPRESSION_BUCKETS);
        while let Some((ts, count)) = self.suppressed_buckets.pop_front() {
            let bucket = ts - ts.rem_euclid(SUPPRESSION_BUCKET_MS);
            if let Some((last_ts, last_count)) = merged.back_mut()
                && *last_ts == bucket
            {
                *last_count = last_count.saturating_add(count);
            } else {
                merged.push_back((bucket, count));
            }
        }
        self.suppressed_buckets = merged;
    }
}

/// Parses a strictly non-negative integer `Retry-After` header value in seconds.
///
/// Rejects negative, empty, non-ASCII-digit, or malformed header values, and clamps huge
/// integers to `MAX_RETRY_AFTER_MS`.
pub fn parse_retry_after_ms(header_value: &str) -> Option<u64> {
    let trimmed = header_value.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let seconds = trimmed.parse::<u64>().unwrap_or(u64::MAX);
    let ms = seconds.saturating_mul(1_000);
    Some(ms.min(MAX_RETRY_AFTER_MS))
}

/// Computes the next retry timestamp using bounded full jitter or `Retry-After`.
///
/// Returns `None` when the three-attempt ceiling or five-minute TTL would be exceeded.
pub fn compute_next_retry_at_ms(
    now_ms: i64,
    attempt_ordinal: u32,
    expires_at_ms: i64,
    retry_after_ms: Option<u64>,
    jitter_unit_sample: f64,
) -> Option<i64> {
    assert!(
        (0.0..=1.0).contains(&jitter_unit_sample),
        "jitter unit sample must be within [0.0, 1.0], got {jitter_unit_sample}"
    );
    if attempt_ordinal >= MAX_ATTEMPTS_PER_INTENT || now_ms >= expires_at_ms {
        return None;
    }
    let delay_ms = retry_after_ms.map_or_else(
        || {
            let shift = attempt_ordinal.saturating_sub(1).min(8);
            let cap = RETRY_BACKOFF_BASE_MS
                .saturating_mul(1_u64 << shift)
                .min(RETRY_BACKOFF_CAP_MS);
            let scaled = (cap as f64) * jitter_unit_sample;
            (scaled.round() as u64).min(cap)
        },
        |value| value.min(MAX_RETRY_AFTER_MS),
    );
    let next_at_ms = now_ms.saturating_add(i64::try_from(delay_ms).unwrap_or(i64::MAX));
    (next_at_ms < expires_at_ms).then_some(next_at_ms)
}

const fn gcra_next_eligible_ms(tat_ms: i64, now_ms: i64) -> Option<i64> {
    let tolerance = NVR_GCRA_TOLERANCE_MS as i64;
    let allowed_tat = now_ms.saturating_add(tolerance);
    if tat_ms <= allowed_tat {
        None
    } else {
        Some(tat_ms.saturating_sub(tolerance))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enforces_gcra_2_per_sec_burst_4_at_exact_boundaries() {
        let mut pacing = LocalPacingState::default();
        for _ in 0..4 {
            assert_eq!(
                pacing.check_production_attempt(1_000, true),
                PaceDecision::Allowed
            );
            pacing.consume_gcra_attempt(1_000);
        }
        assert_eq!(
            pacing.check_production_attempt(1_000, true),
            PaceDecision::Delayed {
                next_eligible_at_ms: 1_500,
                reason: CloudFailureCode::LocalLimit,
            }
        );
        assert_eq!(
            pacing.check_production_attempt(1_499, true),
            PaceDecision::Delayed {
                next_eligible_at_ms: 1_500,
                reason: CloudFailureCode::LocalLimit,
            }
        );
        assert_eq!(
            pacing.check_production_attempt(1_500, true),
            PaceDecision::Allowed
        );
    }

    #[test]
    fn enforces_30s_spacing_120_per_hour_500_per_day_and_separate_3_per_hour_test() {
        let mut pacing = LocalPacingState::default();
        let base = 10_000_000_i64;
        assert_eq!(
            pacing.check_production_attempt(base, false),
            PaceDecision::Allowed
        );
        pacing.consume_gcra_attempt(base);
        pacing.record_production_accepted(base);

        // 30-second spacing blocks earlier non-retry intent.
        assert_eq!(
            pacing.check_production_attempt(base + 29_999, false),
            PaceDecision::Delayed {
                next_eligible_at_ms: base + 30_000,
                reason: CloudFailureCode::LocalLimit,
            }
        );
        assert_eq!(
            pacing.check_production_attempt(base + 30_000, false),
            PaceDecision::Allowed
        );

        // Fill remaining 119 slots in the first hour (120 total).
        for index in 1..NVR_HOURLY_LIMIT {
            let ts = base + i64::try_from(index).unwrap() * 30_000;
            assert_eq!(
                pacing.check_production_attempt(ts, false),
                PaceDecision::Allowed
            );
            pacing.consume_gcra_attempt(ts);
            pacing.record_production_accepted(ts);
        }
        assert_eq!(pacing.accepted_counts(base + 3_599_999), (120, 120));
        // Separate 3/hour test limit works even while production hourly limit is full.
        let test_base = base + 3_590_000;
        for step in 0..3 {
            assert_eq!(
                pacing.try_consume_test_signal(test_base + step * 1_000),
                PaceDecision::Allowed
            );
        }
        assert_eq!(
            pacing.try_consume_test_signal(test_base + 5_000),
            PaceDecision::Delayed {
                next_eligible_at_ms: test_base + ROLLING_HOUR_MS,
                reason: CloudFailureCode::LocalLimit,
            }
        );
        // Test signals do not alter production accepted count.
        assert_eq!(pacing.accepted_counts(test_base + 5_000), (120, 120));

        assert_eq!(
            pacing.check_production_attempt(base + 3_599_999, false),
            PaceDecision::Delayed {
                next_eligible_at_ms: base + ROLLING_HOUR_MS,
                reason: CloudFailureCode::LocalLimit,
            }
        );
        assert_eq!(
            pacing.check_production_attempt(base + ROLLING_HOUR_MS, false),
            PaceDecision::Allowed
        );

        // Fill up to exact 500/day across 5 hours.
        let mut day_pacing = LocalPacingState::default();
        let mut ts = base;
        for index in 0..NVR_DAILY_LIMIT {
            if index > 0 && index % NVR_HOURLY_LIMIT == 0 {
                ts += ROLLING_HOUR_MS + 30_000;
            } else if index > 0 {
                ts += 30_000;
            }
            assert_eq!(
                day_pacing.check_production_attempt(ts, false),
                PaceDecision::Allowed
            );
            day_pacing.consume_gcra_attempt(ts);
            day_pacing.record_production_accepted(ts);
        }
        assert_eq!(day_pacing.accepted_counts(ts + 30_000).1, 500);
        assert_eq!(
            day_pacing.check_production_attempt(ts + ROLLING_HOUR_MS, false),
            PaceDecision::Delayed {
                next_eligible_at_ms: base + ROLLING_DAY_MS,
                reason: CloudFailureCode::LocalLimit,
            }
        );
        assert_eq!(
            day_pacing.check_production_attempt(base + ROLLING_DAY_MS, false),
            PaceDecision::Allowed
        );
    }

    #[test]
    fn clock_rollback_never_replenishes_capacity_or_skips_retry_after() {
        let mut pacing = LocalPacingState::default();
        let now = 50_000_000_i64;
        for index in 0..3 {
            assert_eq!(
                pacing.try_consume_test_signal(now + index * 1_000),
                PaceDecision::Allowed
            );
        }
        // Roll clock backward by 2 hours.
        let rolled_back = now - 2 * ROLLING_HOUR_MS;
        assert!(matches!(
            pacing.try_consume_test_signal(rolled_back),
            PaceDecision::Delayed { .. }
        ));
        assert_eq!(pacing.clock_floor_ms, now + 2_000);
    }

    #[test]
    fn retry_after_parsing_and_jitter_respect_three_attempts_and_five_minute_ttl() {
        assert_eq!(parse_retry_after_ms("15"), Some(15_000));
        assert_eq!(parse_retry_after_ms(" 0 "), Some(0));
        assert_eq!(parse_retry_after_ms("999999999"), Some(MAX_RETRY_AFTER_MS));
        assert_eq!(parse_retry_after_ms("-5"), None);
        assert_eq!(parse_retry_after_ms("abc"), None);
        assert_eq!(parse_retry_after_ms("12.5"), None);
        assert_eq!(parse_retry_after_ms(""), None);

        let created = 1_000_000_i64;
        let expires = created + INTENT_TTL_MS;
        assert_eq!(
            compute_next_retry_at_ms(created + 1_000, 1, expires, None, 0.0),
            Some(created + 1_000)
        );
        assert_eq!(
            compute_next_retry_at_ms(created + 1_000, 1, expires, None, 1.0),
            Some(created + 6_000)
        );
        assert_eq!(
            compute_next_retry_at_ms(created + 1_000, 2, expires, None, 1.0),
            Some(created + 11_000)
        );
        // Third attempt exhaustion returns None.
        assert_eq!(
            compute_next_retry_at_ms(created + 1_000, 3, expires, None, 0.5),
            None
        );
        // Retry-After beyond TTL expires rather than extending TTL.
        assert_eq!(
            compute_next_retry_at_ms(created + 290_000, 1, expires, Some(15_000), 0.5),
            None
        );
        assert_eq!(
            compute_next_retry_at_ms(created + 290_000, 1, expires, Some(9_000), 0.5),
            Some(created + 299_000)
        );
    }

    #[test]
    fn suppression_buckets_stay_bounded_under_high_volume_storms() {
        let mut pacing = LocalPacingState::default();
        let base = 1_000_000_000_i64;
        for index in 0..10_000_i64 {
            pacing.record_suppression(base + index * 10, 1);
        }
        assert!(pacing.suppressed_buckets.len() <= MAX_SUPPRESSION_BUCKETS);
        assert_eq!(pacing.suppressed_counts(base + 100_000), (10_000, 10_000));
    }
}
