//! Bounded scheduler, HTTPS delivery client, and deletion coordinator for usage reporting.
//!
//! Disabled mode starts no background worker and makes zero network requests.

use super::model::{SchemaValidationError, UsageReportV1};
use super::state::{FailureKind, StateError, TelemetryRuntimeState};
use crate::config::usage_reporting::{UsageReportingConfig, validate_receiver_origin};
use rand::RngExt as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const STARTUP_DELAY_MS: i64 = 15 * 60 * 1_000;
pub const REPORT_INTERVAL_MS: i64 = 24 * 60 * 60 * 1_000;
pub const MAX_JITTER_MS: i64 = 60 * 60 * 1_000;
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
pub const MAX_RESPONSE_BYTES: usize = 4 * 1_024;

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| {
                i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
            })
    }
}

pub trait JitterSource: Send + Sync {
    fn non_negative_jitter_ms(&self, max_ms: i64) -> i64;
}

pub struct RandomJitter;

impl JitterSource for RandomJitter {
    fn non_negative_jitter_ms(&self, max_ms: i64) -> i64 {
        if max_ms <= 0 {
            return 0;
        }
        let max_u64 = u64::try_from(max_ms).unwrap_or(0);
        let sample = rand::rng().random_range(0..=max_u64);
        i64::try_from(sample).unwrap_or(max_ms)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportResponse {
    pub status: u16,
}

pub trait TelemetryTransport: Send + Sync {
    fn put_report(
        &self,
        origin: &str,
        installation_id: &str,
        installation_token: &str,
        body: &[u8],
    ) -> Result<TransportResponse, FailureKind>;

    fn delete_installation(
        &self,
        origin: &str,
        installation_id: &str,
        installation_token: &str,
    ) -> Result<TransportResponse, FailureKind>;
}

pub struct HttpsTelemetryTransport {
    agent: ureq::Agent,
}

impl Default for HttpsTelemetryTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpsTelemetryTransport {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .max_response_header_size(16_384)
            .timeout_global(Some(REQUEST_TIMEOUT))
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .provider(ureq::tls::TlsProvider::NativeTls)
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .build(),
            )
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
        }
    }
}

impl TelemetryTransport for HttpsTelemetryTransport {
    fn put_report(
        &self,
        origin: &str,
        installation_id: &str,
        installation_token: &str,
        body: &[u8],
    ) -> Result<TransportResponse, FailureKind> {
        if validate_receiver_origin(origin).is_err() {
            return Err(FailureKind::ReceiverUnconfigured);
        }
        let url = format!("{origin}/v1/installations/{installation_id}");
        let response = self
            .agent
            .put(&url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("Authorization", &format!("Bearer {installation_token}"))
            .send(body)
            .map_err(classify_ureq_error)?;
        read_bounded_response(response)
    }

    fn delete_installation(
        &self,
        origin: &str,
        installation_id: &str,
        installation_token: &str,
    ) -> Result<TransportResponse, FailureKind> {
        if validate_receiver_origin(origin).is_err() {
            return Err(FailureKind::ReceiverUnconfigured);
        }
        let url = format!("{origin}/v1/installations/{installation_id}");
        let response = self
            .agent
            .delete(&url)
            .header("Accept", "application/json")
            .header("Authorization", &format!("Bearer {installation_token}"))
            .call()
            .map_err(classify_ureq_error)?;
        read_bounded_response(response)
    }
}

fn classify_ureq_error(error: ureq::Error) -> FailureKind {
    match error {
        ureq::Error::Timeout(_) => FailureKind::Timeout,
        ureq::Error::HostNotFound => FailureKind::DnsFailed,
        ureq::Error::Tls(_) => FailureKind::TlsFailed,
        ureq::Error::RedirectFailed => FailureKind::InvalidResponse,
        _ => FailureKind::NetworkUnavailable,
    }
}

fn read_bounded_response(
    response: ureq::http::Response<ureq::Body>,
) -> Result<TransportResponse, FailureKind> {
    let status = response.status().as_u16();
    if response.status().is_redirection() {
        return Err(FailureKind::InvalidResponse);
    }
    let bytes = response
        .into_body()
        .into_with_config()
        .limit(MAX_RESPONSE_BYTES as u64 + 1)
        .read_to_vec()
        .map_err(|_| FailureKind::InvalidResponse)?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(FailureKind::InvalidResponse);
    }
    Ok(TransportResponse { status })
}

/// Computes the next allowed send timestamp, clamping clock rollbacks so sends never accelerate.
pub fn compute_next_schedule_ms(
    now_ms: i64,
    started_at_ms: i64,
    last_attempt_at_ms: Option<i64>,
    jitter: &dyn JitterSource,
) -> i64 {
    let extra_jitter = jitter
        .non_negative_jitter_ms(MAX_JITTER_MS)
        .clamp(0, MAX_JITTER_MS);
    let startup_anchor = if now_ms < started_at_ms {
        now_ms
    } else {
        started_at_ms
    };
    let startup_floor = startup_anchor.saturating_add(STARTUP_DELAY_MS);
    let interval_floor = match last_attempt_at_ms {
        Some(last_attempt) if last_attempt <= now_ms => {
            last_attempt.saturating_add(REPORT_INTERVAL_MS)
        }
        Some(_) => {
            // Clock rolled backward relative to persisted last attempt: require a full interval from now.
            now_ms.saturating_add(REPORT_INTERVAL_MS)
        }
        None => startup_floor,
    };
    let base = startup_floor.max(interval_floor);
    base.saturating_add(extra_jitter)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendAttemptOutcome {
    Disabled,
    NotDue,
    Succeeded,
    Failed(FailureKind),
    AbortedConsentChanged,
}

pub struct UsageReportingController {
    state_path: PathBuf,
    started_at_ms: i64,
    clock: Arc<dyn Clock>,
    jitter: Arc<dyn JitterSource>,
    transport: Arc<dyn TelemetryTransport>,
    state: Mutex<TelemetryRuntimeState>,
    sending: AtomicBool,
}

impl UsageReportingController {
    pub fn open(
        state_path: PathBuf,
        clock: Arc<dyn Clock>,
        jitter: Arc<dyn JitterSource>,
        transport: Arc<dyn TelemetryTransport>,
    ) -> Result<Self, StateError> {
        let loaded = TelemetryRuntimeState::load_if_present(&state_path)?;
        let started_at_ms = clock.now_ms();
        Ok(Self {
            state_path,
            started_at_ms,
            clock,
            jitter,
            transport,
            state: Mutex::new(loaded),
            sending: AtomicBool::new(false),
        })
    }

    pub fn snapshot_state(&self) -> TelemetryRuntimeState {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn is_sending(&self) -> bool {
        self.sending.load(Ordering::Acquire)
    }

    /// Synchronizes internal state after a config consent change and performs bounded deletion on disable.
    pub fn apply_consent_transition(
        &self,
        config: &UsageReportingConfig,
    ) -> Result<TelemetryRuntimeState, StateError> {
        let now_ms = self.clock.now_ms();
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if config.any_enabled() {
            let _ = guard.ensure_identity(now_ms);
            if guard.next_scheduled_at_ms.is_none() {
                guard.next_scheduled_at_ms = Some(compute_next_schedule_ms(
                    now_ms,
                    self.started_at_ms,
                    guard.last_attempt_at_ms,
                    self.jitter.as_ref(),
                ));
            }
            guard.save_atomically(&self.state_path)?;
            return Ok(guard.clone());
        }

        let had_identity = guard.identity.is_some();
        if had_identity {
            guard.disable_and_queue_deletion(now_ms)?;
            guard.save_atomically(&self.state_path)?;
        }
        drop(guard);

        if had_identity {
            let _ = self.process_pending_deletions(config.receiver_origin.as_deref());
        }
        Ok(self.snapshot_state())
    }

    /// Rotates the dedicated telemetry UUID and immediately attempts bounded deletion of the prior row.
    pub fn reset_identity(
        &self,
        config: &UsageReportingConfig,
    ) -> Result<TelemetryRuntimeState, StateError> {
        let now_ms = self.clock.now_ms();
        {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.reset_identity(now_ms, config.any_enabled())?;
            if config.any_enabled() {
                guard.next_scheduled_at_ms = Some(compute_next_schedule_ms(
                    now_ms,
                    self.started_at_ms,
                    guard.last_attempt_at_ms,
                    self.jitter.as_ref(),
                ));
            }
            guard.save_atomically(&self.state_path)?;
        }
        let _ = self.process_pending_deletions(config.receiver_origin.as_deref());
        Ok(self.snapshot_state())
    }

    /// Explicitly retries every pending deletion against the configured HTTPS receiver.
    pub fn process_pending_deletions(
        &self,
        receiver_origin: Option<&str>,
    ) -> Result<TelemetryRuntimeState, StateError> {
        let now_ms = self.clock.now_ms();
        let pending = {
            let guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.pending_deletions.clone()
        };
        if pending.is_empty() {
            return Ok(self.snapshot_state());
        }
        let Some(origin) = receiver_origin else {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.last_deletion_attempt_at_ms = Some(now_ms);
            guard.last_deletion_failure = FailureKind::ReceiverUnconfigured;
            guard.save_atomically(&self.state_path)?;
            return Ok(guard.clone());
        };

        let mut remaining = Vec::new();
        let mut last_failure = FailureKind::None;
        let mut any_success = false;
        for entry in pending {
            match self.transport.delete_installation(
                origin,
                &entry.installation_id,
                &entry.installation_token,
            ) {
                Ok(response) if matches!(response.status, 200 | 204 | 404 | 410) => {
                    any_success = true;
                }
                Ok(response) => {
                    last_failure = classify_http_status(response.status, true);
                    remaining.push(entry);
                }
                Err(kind) => {
                    last_failure = kind;
                    remaining.push(entry);
                }
            }
        }

        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.last_deletion_attempt_at_ms = Some(now_ms);
        if any_success && remaining.is_empty() {
            guard.last_deletion_success_at_ms = Some(now_ms);
            guard.last_deletion_failure = FailureKind::None;
        } else if !matches!(last_failure, FailureKind::None) {
            guard.last_deletion_failure = last_failure;
        }
        guard.pending_deletions = remaining;
        guard.save_atomically(&self.state_path)?;
        Ok(guard.clone())
    }

    /// Runs one bounded scheduled send check; never retries within the same interval on failure.
    pub fn maybe_send_due<F, C>(
        &self,
        config: &UsageReportingConfig,
        build_report: F,
        still_consented: C,
    ) -> SendAttemptOutcome
    where
        F: FnOnce(&str, u64) -> Result<UsageReportV1, SchemaValidationError>,
        C: Fn() -> bool,
    {
        if !config.any_enabled() {
            return SendAttemptOutcome::Disabled;
        }
        let now_ms = self.clock.now_ms();
        let Some((installation_id, installation_token, next_sequence)) =
            self.prepare_due_send(now_ms)
        else {
            return SendAttemptOutcome::NotDue;
        };

        if self
            .sending
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return SendAttemptOutcome::NotDue;
        }
        let _sending_guard = SendingFlagReset(&self.sending);

        let payload_json = match build_report(&installation_id, next_sequence)
            .and_then(|report| report.to_bounded_json())
        {
            Ok(json) => json,
            Err(error) => {
                let failure = match error {
                    SchemaValidationError::PayloadTooLarge { .. } => FailureKind::PayloadTooLarge,
                    _ => FailureKind::ValidationFailed,
                };
                self.record_attempt_outcome(now_ms, &installation_id, next_sequence, Err(failure));
                return SendAttemptOutcome::Failed(failure);
            }
        };

        // Recheck consent and active identity immediately before touching the network.
        if !still_consented() || !self.identity_is_still_active(&installation_id) {
            return SendAttemptOutcome::AbortedConsentChanged;
        }

        let result = self.deliver_report_bytes(
            config.receiver_origin.as_deref(),
            &installation_id,
            &installation_token,
            payload_json.as_bytes(),
        );

        self.record_attempt_outcome(now_ms, &installation_id, next_sequence, result);
        match result {
            Ok(()) => SendAttemptOutcome::Succeeded,
            Err(kind) => SendAttemptOutcome::Failed(kind),
        }
    }

    fn deliver_report_bytes(
        &self,
        origin: Option<&str>,
        installation_id: &str,
        installation_token: &str,
        body: &[u8],
    ) -> Result<(), FailureKind> {
        let Some(origin) = origin else {
            return Err(FailureKind::ReceiverUnconfigured);
        };
        match self
            .transport
            .put_report(origin, installation_id, installation_token, body)
        {
            Ok(response) if matches!(response.status, 200 | 201 | 204) => Ok(()),
            Ok(response) => Err(classify_http_status(response.status, false)),
            Err(kind) => Err(kind),
        }
    }

    fn prepare_due_send(&self, now_ms: i64) -> Option<(String, String, u64)> {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(last_attempt) = guard.last_attempt_at_ms
            && now_ms < last_attempt
        {
            // Clamp rollback once so sends never accelerate, while allowing recovery after a full interval.
            guard.last_attempt_at_ms = Some(now_ms);
            guard.next_scheduled_at_ms = Some(compute_next_schedule_ms(
                now_ms,
                self.started_at_ms,
                Some(now_ms),
                self.jitter.as_ref(),
            ));
            let _ = guard.save_atomically(&self.state_path);
            return None;
        }
        let due_at = guard.next_scheduled_at_ms.unwrap_or_else(|| {
            let computed = compute_next_schedule_ms(
                now_ms,
                self.started_at_ms,
                guard.last_attempt_at_ms,
                self.jitter.as_ref(),
            );
            guard.next_scheduled_at_ms = Some(computed);
            let _ = guard.save_atomically(&self.state_path);
            computed
        });
        if now_ms < due_at {
            return None;
        }
        let identity = guard.ensure_identity(now_ms).clone();
        let next_sequence = identity.report_sequence.saturating_add(1);
        Some((
            identity.installation_id,
            identity.installation_token,
            next_sequence,
        ))
    }

    fn identity_is_still_active(&self, installation_id: &str) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .identity
            .as_ref()
            .is_some_and(|identity| identity.installation_id == installation_id)
    }

    fn record_attempt_outcome(
        &self,
        now_ms: i64,
        installation_id: &str,
        attempted_sequence: u64,
        outcome: Result<(), FailureKind>,
    ) {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.last_attempt_at_ms = Some(now_ms);
        if let Some(identity) = guard.identity.as_mut()
            && identity.installation_id == installation_id
        {
            identity.report_sequence = identity.report_sequence.max(attempted_sequence);
        }
        match outcome {
            Ok(()) => {
                guard.last_success_at_ms = Some(now_ms);
                guard.last_failure = FailureKind::None;
            }
            Err(kind) => {
                guard.last_failure = kind;
            }
        }
        if guard.identity.is_some() {
            guard.next_scheduled_at_ms = Some(compute_next_schedule_ms(
                now_ms,
                self.started_at_ms,
                Some(now_ms),
                self.jitter.as_ref(),
            ));
        }
        let _ = guard.save_atomically(&self.state_path);
    }
}

struct SendingFlagReset<'a>(&'a AtomicBool);

impl Drop for SendingFlagReset<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

const fn classify_http_status(status: u16, is_deletion: bool) -> FailureKind {
    match status {
        429 => FailureKind::RateLimited,
        400..=499 => {
            if is_deletion {
                FailureKind::DeletionFailed
            } else {
                FailureKind::ReceiverRejected
            }
        }
        500..=599 => FailureKind::ReceiverUnavailable,
        _ => FailureKind::InvalidResponse,
    }
}
