//! Compact HTTPS transport and background worker for Keeppeek Service event signals.
//!
//! Sends exact 109-byte `application/octet-stream` payloads to `POST /api/v1/nvr/events`
//! with strict timeouts, zero redirects, no bearer credentials, and bounded discarded responses.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rand::RngExt as _;
use uuid::Uuid;

use crate::config::{
    self,
    cloud_notifications::{PairingReference, Settings, SigningSeed, validate_service_origin},
};

use super::{
    cloud_limits::{CloudFailureCode, PaceDecision, parse_retry_after_ms},
    cloud_protocol::{CANONICAL_BODY_BYTES, SignalEnvelope, sign_signal},
    cloud_state::{
        CloudEventAlias, CloudStateStore, CloudStatusSnapshot, EnqueueOutcome, PairingIdentity,
        SignalDeliveryOutcome, SigningMaterial,
    },
};

/// Path appended to the canonical HTTPS Keeppeek Service origin.
pub const SIGNAL_ENDPOINT_PATH: &str = "/api/v1/nvr/events";

/// Global HTTP timeout for one outbound Keeppeek Service request.
pub const CLOUD_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum response body bytes read and discarded from Keeppeek Service.
pub const CLOUD_MAX_RESPONSE_BYTES: u64 = 4 * 1_024;

/// Idle sleep interval for the cloud signaling worker when no intent is due.
const CLOUD_WORKER_IDLE_INTERVAL: Duration = Duration::from_millis(100);

/// Abstract outbound transport so production HTTPS and deterministic fakes share one worker path.
pub trait SignalTransport: Send + Sync {
    fn send_signal(
        &self,
        service_origin: &str,
        body: &[u8; CANONICAL_BODY_BYTES],
    ) -> SignalDeliveryOutcome;
}

/// Production HTTPS transport using `ureq` with zero redirects and strict timeout/response caps.
#[derive(Clone)]
pub struct HttpsSignalTransport {
    agent: ureq::Agent,
    allow_insecure_test_origin: bool,
}

impl HttpsSignalTransport {
    /// Creates a production HTTPS transport that rejects non-HTTPS origins and all redirects.
    pub fn new() -> Self {
        Self::with_timeout(CLOUD_REQUEST_TIMEOUT, false)
    }

    /// Creates a transport with a custom timeout and optional loopback HTTP override for tests.
    pub fn with_timeout(timeout: Duration, allow_insecure_test_origin: bool) -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(timeout))
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
            allow_insecure_test_origin,
        }
    }
}

impl Default for HttpsSignalTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl SignalTransport for HttpsSignalTransport {
    fn send_signal(
        &self,
        service_origin: &str,
        body: &[u8; CANONICAL_BODY_BYTES],
    ) -> SignalDeliveryOutcome {
        if !self.allow_insecure_test_origin && validate_service_origin(service_origin).is_err() {
            return SignalDeliveryOutcome::BadRequest;
        }
        let endpoint = format!("{service_origin}{SIGNAL_ENDPOINT_PATH}");
        let response = self
            .agent
            .post(&endpoint)
            .header("Content-Type", "application/octet-stream")
            .send(body.as_slice());
        classify_http_result(response)
    }
}

/// Classifies an HTTP response or transport error into the closed Keeppeek Service outcome set.
fn classify_http_result(
    response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> SignalDeliveryOutcome {
    response.map_or(SignalDeliveryOutcome::DependencyFailure, |mut response| {
        let status = response.status().as_u16();
        let retry_after_ms = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(parse_retry_after_ms);
        // Drain at most CLOUD_MAX_RESPONSE_BYTES and discard raw text to avoid storing provider text.
        let _ = response
            .body_mut()
            .with_config()
            .limit(CLOUD_MAX_RESPONSE_BYTES)
            .read_to_vec();
        classify_http_status(status, retry_after_ms)
    })
}

/// Maps HTTP status codes to the exact response handling table in issue #213.
pub const fn classify_http_status(
    status: u16,
    retry_after_ms: Option<u64>,
) -> SignalDeliveryOutcome {
    match status {
        202 => SignalDeliveryOutcome::Accepted,
        400 => SignalDeliveryOutcome::BadRequest,
        401 | 403 => SignalDeliveryOutcome::CredentialRejected,
        409 => SignalDeliveryOutcome::IntegrityConflict,
        429 => SignalDeliveryOutcome::RateLimited { retry_after_ms },
        503 => SignalDeliveryOutcome::ServiceUnavailable { retry_after_ms },
        _ => SignalDeliveryOutcome::DependencyFailure,
    }
}

/// Outcome returned from an explicit administrator test signal request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TestSignalOutcome {
    pub accepted: bool,
    pub next_eligible_at_ms: Option<i64>,
    pub failure: CloudFailureCode,
}

#[derive(Clone, Debug)]
struct StagedCloudEvent {
    logical_id: String,
    rule_id: String,
    event_id: String,
    source_id: String,
    event_revision: u64,
    now_ms: i64,
    coalesced_replacements: u32,
}

/// Shared coordinator binding durable cloud state, live configuration, and outbound transport.
#[derive(Clone)]
pub struct CloudSignalRuntime {
    config_path: Arc<PathBuf>,
    settings: Arc<RwLock<Settings>>,
    active_gate: Arc<AtomicBool>,
    staged: Arc<Mutex<Option<StagedCloudEvent>>>,
    state: CloudStateStore,
    transport: Arc<dyn SignalTransport>,
    metrics: Arc<CloudMetrics>,
}

/// Bounded counter-only metrics for cloud signaling (no IDs, URLs, credentials, or text).
#[derive(Debug, Default)]
pub struct CloudMetrics {
    pub signals_enqueued: std::sync::atomic::AtomicU64,
    pub signals_replaced: std::sync::atomic::AtomicU64,
    pub signals_attempted: std::sync::atomic::AtomicU64,
    pub signals_accepted: std::sync::atomic::AtomicU64,
    pub signals_failed: std::sync::atomic::AtomicU64,
    pub signals_suppressed: std::sync::atomic::AtomicU64,
}

/// Immutable snapshot of bounded cloud signaling metrics for Prometheus exposition.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CloudMetricsSnapshot {
    pub state: Option<super::cloud_limits::CloudState>,
    pub signals_enqueued: u64,
    pub signals_replaced: u64,
    pub signals_attempted: u64,
    pub signals_accepted: u64,
    pub signals_failed: u64,
    pub signals_suppressed: u64,
}

impl CloudSignalRuntime {
    /// Opens durable cloud notification state with the default HTTPS transport.
    pub fn open(
        config_path: PathBuf,
        state_db_path: &std::path::Path,
        settings: Settings,
        now_ms: i64,
    ) -> anyhow::Result<Self> {
        Self::open_with_transport(
            config_path,
            state_db_path,
            settings,
            Arc::new(HttpsSignalTransport::new()),
            now_ms,
        )
    }

    /// Opens durable cloud notification state with a caller-supplied transport (for tests).
    pub fn open_with_transport(
        config_path: PathBuf,
        state_db_path: &std::path::Path,
        settings: Settings,
        transport: Arc<dyn SignalTransport>,
        now_ms: i64,
    ) -> anyhow::Result<Self> {
        settings.validate()?;
        let active = settings.enabled && settings.is_paired();
        let state = CloudStateStore::open(state_db_path, now_ms)?;
        state.reconcile_pairing(pairing_identity(&settings)?, now_ms)?;
        Ok(Self {
            config_path: Arc::new(config_path),
            settings: Arc::new(RwLock::new(settings)),
            active_gate: Arc::new(AtomicBool::new(active)),
            staged: Arc::new(Mutex::new(None)),
            state,
            transport,
            metrics: Arc::new(CloudMetrics::default()),
        })
    }

    /// Returns a copy of the current validated settings without resolving secrets.
    pub fn settings(&self) -> Settings {
        self.settings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Replaces in-memory settings after an atomic configuration commit.
    pub fn apply_settings(&self, next: Settings, now_ms: i64) -> anyhow::Result<()> {
        next.validate()?;
        let next_pairing = pairing_identity(&next)?;
        let active = next.enabled && next.is_paired();
        self.active_gate.store(active, Ordering::Release);
        *self
            .settings
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = next;
        self.state.reconcile_pairing(next_pairing, now_ms)?;
        if !active {
            *self
                .staged
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            self.state.cancel_pending_on_disable(now_ms)?;
        }
        Ok(())
    }

    /// Coalesces a matched logical notification in O(1) memory without blocking on disk fsync.
    ///
    /// At most one newest candidate stays staged until flushed durably before claiming or status.
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors the logical notification identity fields needed for alias creation"
    )]
    pub fn stage_logical_event(
        &self,
        logical_id: &str,
        rule_id: &str,
        event_id: &str,
        source_id: &str,
        event_revision: u64,
        now_ms: i64,
    ) -> bool {
        if !self.active_gate.load(Ordering::Acquire) {
            return false;
        }
        let mut slot = self
            .staged
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (replaced, coalesced_replacements) = match slot.as_ref() {
            Some(previous) if previous.logical_id != logical_id => {
                (true, previous.coalesced_replacements.saturating_add(1))
            }
            Some(previous) => (false, previous.coalesced_replacements),
            None => (false, 0),
        };
        *slot = Some(StagedCloudEvent {
            logical_id: logical_id.to_owned(),
            rule_id: rule_id.to_owned(),
            event_id: event_id.to_owned(),
            source_id: source_id.to_owned(),
            event_revision,
            now_ms,
            coalesced_replacements,
        });
        drop(slot);
        self.metrics
            .signals_enqueued
            .fetch_add(1, Ordering::Relaxed);
        if replaced {
            self.metrics
                .signals_replaced
                .fetch_add(1, Ordering::Relaxed);
            self.metrics
                .signals_suppressed
                .fetch_add(1, Ordering::Relaxed);
        }
        true
    }

    /// Commits any staged newest-only event into durable Turso storage.
    pub fn flush_staged(&self) -> anyhow::Result<Option<EnqueueOutcome>> {
        let staged = self
            .staged
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let Some(staged) = staged else {
            return Ok(None);
        };
        if !self.active_gate.load(Ordering::Acquire) {
            return Ok(None);
        }
        let outcome = self.state.enqueue_logical_event_with_coalesced(
            &staged.logical_id,
            &staged.rule_id,
            &staged.event_id,
            &staged.source_id,
            staged.event_revision,
            staged.now_ms,
            staged.coalesced_replacements,
        )?;
        if outcome.replaced_previous {
            self.metrics
                .signals_replaced
                .fetch_add(1, Ordering::Relaxed);
            self.metrics
                .signals_suppressed
                .fetch_add(1, Ordering::Relaxed);
        }
        Ok(Some(outcome))
    }

    /// Synchronously enqueues a matched logical notification into durable cloud state.
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors the logical notification identity fields needed for alias creation"
    )]
    pub fn enqueue_logical_event(
        &self,
        logical_id: &str,
        rule_id: &str,
        event_id: &str,
        source_id: &str,
        event_revision: u64,
        now_ms: i64,
    ) -> anyhow::Result<Option<EnqueueOutcome>> {
        if !self.active_gate.load(Ordering::Acquire) {
            return Ok(None);
        }
        let _ = self.flush_staged()?;
        let outcome = self.state.enqueue_logical_event(
            logical_id,
            rule_id,
            event_id,
            source_id,
            event_revision,
            now_ms,
        )?;
        if outcome.queued {
            self.metrics
                .signals_enqueued
                .fetch_add(1, Ordering::Relaxed);
        }
        if outcome.replaced_previous {
            self.metrics
                .signals_replaced
                .fetch_add(1, Ordering::Relaxed);
            self.metrics
                .signals_suppressed
                .fetch_add(1, Ordering::Relaxed);
        }
        Ok(Some(outcome))
    }

    /// Runs one bounded claim-send-finish step using `raw_now_ms` and `jitter_unit_sample`.
    ///
    /// Never holds the durable state lock while performing network I/O.
    pub fn step_once(&self, raw_now_ms: i64, jitter_unit_sample: f64) -> anyhow::Result<bool> {
        let current = self.settings();
        if !current.enabled || !current.is_paired() {
            return Ok(false);
        }
        let can_claim = self.state.can_claim_now(true, true, raw_now_ms);
        let (pending_depth, claimed_depth) = self.state.queue_depth();
        // Flush staged work when eligible to claim or once to populate an empty durable pending slot.
        if can_claim || pending_depth == 0 {
            let _ = self.flush_staged()?;
        }
        if !can_claim {
            return Ok(false);
        }
        if pending_depth == 0 && claimed_depth == 0 && self.state.queue_depth() == (0, 0) {
            return Ok(false);
        }
        let Some(pairing) = current.pairing_reference()? else {
            return Ok(false);
        };
        let seed = match self.resolve_signing_seed(&pairing) {
            Ok(seed) => seed,
            Err(_) => {
                let _ = self
                    .state
                    .record_test_outcome(SignalDeliveryOutcome::CredentialRejected, raw_now_ms);
                return Ok(false);
            }
        };
        let Some(claimed) = self.state.claim_next_attempt(
            true,
            Some(SigningMaterial {
                nvr_id: pairing.nvr_id,
                signing_key_version: pairing.signing_key_version,
                seed: seed.as_bytes(),
            }),
            raw_now_ms,
        )?
        else {
            return Ok(false);
        };
        self.deliver_claimed_attempt(claimed, &pairing, raw_now_ms, jitter_unit_sample)
    }

    fn deliver_claimed_attempt(
        &self,
        claimed: super::cloud_state::ClaimedAttempt,
        pairing: &PairingReference,
        raw_now_ms: i64,
        jitter_unit_sample: f64,
    ) -> anyhow::Result<bool> {
        self.metrics
            .signals_attempted
            .fetch_add(1, Ordering::Relaxed);
        let outcome = self
            .transport
            .send_signal(&pairing.service_origin, &claimed.canonical_body);
        let after = self.settings();
        let still_enabled = after.enabled
            && after.is_paired()
            && after.service_origin.as_deref() == Some(pairing.service_origin.as_str());
        let finished = self.state.finish_attempt(
            claimed.cloud_event_id,
            claimed.attempt_ordinal,
            outcome,
            still_enabled,
            raw_now_ms,
            jitter_unit_sample,
        )?;
        if finished.accepted {
            self.metrics
                .signals_accepted
                .fetch_add(1, Ordering::Relaxed);
        } else if finished.terminal {
            self.metrics.signals_failed.fetch_add(1, Ordering::Relaxed);
            self.metrics
                .signals_suppressed
                .fetch_add(1, Ordering::Relaxed);
        }
        tracing::info!(
            event = "cloud_notification_attempt",
            attempt = claimed.attempt_ordinal,
            accepted = finished.accepted,
            terminal = finished.terminal,
            failure = finished.failure.as_str()
        );
        Ok(true)
    }

    /// Sends one separately rate-limited (3/hour) test signal without touching production counts.
    pub fn send_test_signal(&self, raw_now_ms: i64) -> anyhow::Result<TestSignalOutcome> {
        let current = self.settings();
        let Some(pairing) = (current.enabled && current.is_paired())
            .then(|| current.pairing_reference())
            .transpose()?
            .flatten()
        else {
            return Ok(TestSignalOutcome {
                accepted: false,
                next_eligible_at_ms: None,
                failure: CloudFailureCode::Dependency,
            });
        };
        if self.status(raw_now_ms).state == super::cloud_limits::CloudState::Unavailable {
            return Ok(TestSignalOutcome {
                accepted: false,
                next_eligible_at_ms: None,
                failure: CloudFailureCode::CredentialRejected,
            });
        }
        let (decision, effective_now_ms) = self.state.reserve_test_signal(raw_now_ms)?;
        if let PaceDecision::Delayed {
            next_eligible_at_ms,
            reason,
        } = decision
        {
            return Ok(TestSignalOutcome {
                accepted: false,
                next_eligible_at_ms: Some(next_eligible_at_ms),
                failure: reason,
            });
        }
        self.sign_and_send_test_envelope(&pairing, effective_now_ms)
    }

    fn sign_and_send_test_envelope(
        &self,
        pairing: &PairingReference,
        effective_now_ms: i64,
    ) -> anyhow::Result<TestSignalOutcome> {
        let Ok(seed) = self.resolve_signing_seed(pairing) else {
            let failure = self
                .state
                .record_test_outcome(SignalDeliveryOutcome::CredentialRejected, effective_now_ms)?;
            return Ok(TestSignalOutcome {
                accepted: false,
                next_eligible_at_ms: None,
                failure,
            });
        };
        let issued_at_secs = u64::try_from(effective_now_ms.max(0) / 1_000)
            .expect("non-negative Unix seconds must fit in u64");
        let body = sign_signal(
            SignalEnvelope {
                nvr_id: pairing.nvr_id,
                cloud_event_id: *Uuid::new_v4().as_bytes(),
                issued_at_secs,
                signing_key_version: pairing.signing_key_version,
            },
            seed.as_bytes(),
        )?;
        self.metrics
            .signals_attempted
            .fetch_add(1, Ordering::Relaxed);
        let outcome = self.transport.send_signal(&pairing.service_origin, &body);
        let failure = self.state.record_test_outcome(outcome, effective_now_ms)?;
        let accepted = outcome == SignalDeliveryOutcome::Accepted;
        if accepted {
            self.metrics
                .signals_accepted
                .fetch_add(1, Ordering::Relaxed);
        } else {
            self.metrics.signals_failed.fetch_add(1, Ordering::Relaxed);
        }
        let next_eligible_at_ms = match outcome {
            SignalDeliveryOutcome::RateLimited {
                retry_after_ms: Some(delay),
            }
            | SignalDeliveryOutcome::ServiceUnavailable {
                retry_after_ms: Some(delay),
            } => Some(effective_now_ms.saturating_add(i64::try_from(delay).unwrap_or(i64::MAX))),
            _ => None,
        };
        Ok(TestSignalOutcome {
            accepted,
            next_eligible_at_ms,
            failure,
        })
    }

    /// Resolves one opaque 16-byte `cloud_event_id` when cloud previews are enabled.
    pub fn resolve_alias(
        &self,
        cloud_event_id: &[u8; 16],
        raw_now_ms: i64,
    ) -> anyhow::Result<Option<CloudEventAlias>> {
        let _ = self.flush_staged()?;
        self.state.resolve_alias(cloud_event_id, raw_now_ms)
    }

    /// Returns the bounded health and suppression snapshot for status reporting.
    pub fn status(&self, raw_now_ms: i64) -> CloudStatusSnapshot {
        let _ = self.flush_staged();
        let current = self.settings();
        self.state
            .status_snapshot(current.enabled, current.is_paired(), raw_now_ms)
    }

    /// Returns a bounded counter snapshot for Prometheus metrics exposition.
    pub fn metrics_snapshot(&self, raw_now_ms: i64) -> CloudMetricsSnapshot {
        let status = self.status(raw_now_ms);
        CloudMetricsSnapshot {
            state: Some(status.state),
            signals_enqueued: self.metrics.signals_enqueued.load(Ordering::Relaxed),
            signals_replaced: self.metrics.signals_replaced.load(Ordering::Relaxed),
            signals_attempted: self.metrics.signals_attempted.load(Ordering::Relaxed),
            signals_accepted: self.metrics.signals_accepted.load(Ordering::Relaxed),
            signals_failed: self.metrics.signals_failed.load(Ordering::Relaxed),
            signals_suppressed: self.metrics.signals_suppressed.load(Ordering::Relaxed),
        }
    }

    /// Returns direct access to the durable store for queue-depth inspection in tests.
    pub const fn store(&self) -> &CloudStateStore {
        &self.state
    }

    fn resolve_signing_seed(&self, pairing: &PairingReference) -> anyhow::Result<SigningSeed> {
        let resolved =
            config::resolve_secret_references(&self.config_path, &pairing.signing_key_reference)?;
        SigningSeed::parse(&resolved)
    }
}

/// Dedicated background worker loop for Keeppeek Service signals, isolated from other channels.
pub struct CloudSignalWorker {
    shutdown: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl CloudSignalWorker {
    /// Spawns one bounded worker thread that drains due cloud intents until shut down.
    pub fn start(runtime: CloudSignalRuntime) -> anyhow::Result<Self> {
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = shutdown.clone();
        let thread = std::thread::Builder::new()
            .name("notification-keeppeek-service".to_owned())
            .spawn(move || {
                while !worker_shutdown.load(Ordering::Acquire) {
                    let jitter: f64 = rand::rng().random_range(0.0..=1.0);
                    match runtime.step_once(unix_time_ms(), jitter) {
                        Ok(true) => {}
                        Ok(false) => std::thread::sleep(CLOUD_WORKER_IDLE_INTERVAL),
                        Err(error) => {
                            tracing::warn!(
                                event = "cloud_notification_worker_error",
                                error = %error,
                                "cloud notification worker step failed"
                            );
                            std::thread::sleep(CLOUD_WORKER_IDLE_INTERVAL);
                        }
                    }
                }
                let _ = runtime.flush_staged();
            })?;
        Ok(Self {
            shutdown,
            thread: Some(thread),
        })
    }

    /// Signals the worker thread to exit and waits for bounded termination.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("cloud notification worker thread panicked");
        }
    }
}

impl Drop for CloudSignalWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn pairing_identity(settings: &Settings) -> anyhow::Result<Option<PairingIdentity>> {
    Ok(settings
        .pairing_reference()?
        .map(|pairing| PairingIdentity {
            service_origin: pairing.service_origin,
            nvr_id: pairing.nvr_id,
            signing_key_version: pairing.signing_key_version,
            signing_key_ref: settings.signing_key.clone(),
        }))
}

fn unix_time_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}
