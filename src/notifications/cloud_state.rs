//! Durable Turso-backed state for bounded Keeppeek Service mobile push signaling.
//!
//! Stores random 128-bit `cloud_event_id` aliases, one newest-only pending intent, one
//! claimed/in-flight canonical 109-byte intent, exact rolling acceptance and test windows,
//! and monotonic clock floor so restarts never replay accepted intents or replenish capacity.

use std::{
    collections::VecDeque,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    cloud_limits::{
        CloudFailureCode, CloudState, INTENT_TTL_MS, LocalPacingState, MAX_ATTEMPTS_PER_INTENT,
        NVR_DAILY_LIMIT, PaceDecision, compute_next_retry_at_ms,
    },
    cloud_protocol::{CANONICAL_BODY_BYTES, SignalEnvelope, sign_signal},
};

/// Durable schema version stored in the singleton runtime state row.
const CLOUD_STATE_SCHEMA_VERSION: u32 = 1;

/// Maximum number of retained `cloud_event_id` aliases in durable storage.
pub const MAX_CLOUD_ALIASES: usize = 2_000;

/// Retention horizon for opaque `cloud_event_id` aliases (7 days).
pub const ALIAS_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1_000;

/// Claim lease duration while an outbound HTTPS request is in flight (15 seconds).
pub const CLAIM_LEASE_MS: i64 = 15_000;

const _: () = {
    assert!(MAX_CLOUD_ALIASES >= NVR_DAILY_LIMIT);
    assert!(CLAIM_LEASE_MS > 0 && CLAIM_LEASE_MS < INTENT_TTL_MS);
};

/// Injectable commit fault flag used to verify rollback on durable write failure.
#[derive(Clone, Debug, Default)]
pub struct CommitFaultHook {
    fail_next: Arc<AtomicBool>,
}

impl CommitFaultHook {
    /// Arms a single failure on the next durable commit.
    pub fn arm_next_failure(&self) {
        self.fail_next.store(true, Ordering::Release);
    }

    fn take(&self) -> bool {
        self.fail_next.swap(false, Ordering::AcqRel)
    }
}

/// Durable alias mapping one random 128-bit `cloud_event_id` to a logical event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudEventAlias {
    pub cloud_event_id: [u8; 16],
    pub logical_id: String,
    pub event_id: String,
    pub source_id: String,
    pub event_revision: u64,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
}

/// Newest-only pending production signal waiting for pacing eligibility.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCloudIntent {
    pub cloud_event_id: [u8; 16],
    pub logical_id: String,
    pub rule_id: String,
    pub revision: u64,
    pub enqueued_at_ms: i64,
    pub expires_at_ms: i64,
}

/// Claimed production intent holding exact signed 109-byte canonical payload across retries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimedCloudIntent {
    pub cloud_event_id: [u8; 16],
    pub logical_id: String,
    pub rule_id: String,
    pub revision: u64,
    #[serde(with = "canonical_body_serde")]
    pub canonical_body: [u8; CANONICAL_BODY_BYTES],
    pub signing_key_version: u32,
    pub nvr_id: [u8; 16],
    pub attempt_count: u32,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
    pub next_attempt_at_ms: i64,
    pub claimed_lease_until_ms: Option<i64>,
    pub in_flight: bool,
}

/// Claimed attempt handed to the outbound HTTPS worker outside any store lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimedAttempt {
    pub cloud_event_id: [u8; 16],
    pub logical_id: String,
    pub rule_id: String,
    pub revision: u64,
    pub attempt_ordinal: u32,
    pub canonical_body: [u8; CANONICAL_BODY_BYTES],
    pub expires_at_ms: i64,
}

/// Result of enqueueing a logical event candidate into newest-only cloud state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnqueueOutcome {
    pub cloud_event_id: [u8; 16],
    pub queued: bool,
    pub replaced_previous: bool,
    pub already_accepted: bool,
}

/// Result of classifying one Keeppeek Service HTTP response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignalDeliveryOutcome {
    Accepted,
    BadRequest,
    CredentialRejected,
    IntegrityConflict,
    RateLimited { retry_after_ms: Option<u64> },
    ServiceUnavailable { retry_after_ms: Option<u64> },
    DependencyFailure,
}

/// Durable completion record emitted for history and bounded metrics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FinishOutcome {
    pub terminal: bool,
    pub accepted: bool,
    pub failure: CloudFailureCode,
    pub next_attempt_at_ms: Option<i64>,
}

/// Snapshot of bounded cloud notification health returned to control API and metrics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloudStatusSnapshot {
    pub state: CloudState,
    pub paired: bool,
    pub pending: bool,
    pub accepted_last_hour: u32,
    pub accepted_last_day: u32,
    pub suppressed_last_hour: u32,
    pub suppressed_last_day: u32,
    pub next_eligible_at_ms: Option<i64>,
    pub last_accepted_at_ms: Option<i64>,
    pub last_failure: CloudFailureCode,
}

/// Parameters required to sign a freshly claimed signal intent.
#[derive(Clone, Copy)]
pub struct SigningMaterial<'a> {
    pub nvr_id: [u8; 16],
    pub signing_key_version: u32,
    pub seed: &'a [u8; 32],
}

/// Non-secret pairing metadata tracked across restarts to detect credential repair.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingIdentity {
    pub service_origin: String,
    pub nvr_id: [u8; 16],
    pub signing_key_version: u32,
    #[serde(default)]
    pub signing_key_ref: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct PersistedCloudRuntime {
    schema_version: u32,
    #[serde(default)]
    pairing_identity: Option<PairingIdentity>,
    pacing: LocalPacingState,
    pending: Option<PendingCloudIntent>,
    claimed: Option<ClaimedCloudIntent>,
    accepted_ids: VecDeque<[u8; 16]>,
}

impl PersistedCloudRuntime {
    fn fresh() -> Self {
        Self {
            schema_version: CLOUD_STATE_SCHEMA_VERSION,
            ..Self::default()
        }
    }
}

struct DurableInner {
    connection: turso::Connection,
    runtime: PersistedCloudRuntime,
    commit_fault: CommitFaultHook,
}

/// Thread-safe handle to the durable cloud notification state database.
#[derive(Clone)]
pub struct CloudStateStore {
    inner: Arc<Mutex<DurableInner>>,
}

impl CloudStateStore {
    /// Opens or creates the durable cloud notification database and recovers in-flight state.
    pub fn open(path: &Path, now_ms: i64) -> anyhow::Result<Self> {
        Self::open_with_fault(path, now_ms, CommitFaultHook::default())
    }

    /// Opens the durable database with an injectable commit-fault hook for crash testing.
    pub fn open_with_fault(
        path: &Path,
        now_ms: i64,
        commit_fault: CommitFaultHook,
    ) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let path_str = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("cloud notification state path is not valid UTF-8"))?;
        let database = pollster::block_on(turso::Builder::new_local(path_str).build())?;
        let connection = database.connect()?;
        pollster::block_on(initialize_schema(&connection))?;
        let mut runtime = pollster::block_on(load_runtime(&connection))?;
        let effective_now = runtime.pacing.clamp_now(now_ms);
        recover_in_flight_on_startup(&mut runtime, effective_now);
        pollster::block_on(save_runtime_tx(
            &connection,
            &runtime,
            &CommitFaultHook::default(),
        ))?;
        pollster::block_on(prune_aliases_tx(&connection, effective_now))?;
        Ok(Self {
            inner: Arc::new(Mutex::new(DurableInner {
                connection,
                runtime,
                commit_fault,
            })),
        })
    }

    /// Persists one random 128-bit alias for `logical_id` and updates the single pending slot.
    #[allow(
        clippy::too_many_arguments,
        reason = "event identity fields map directly to the durable alias and pending intent schema"
    )]
    pub fn enqueue_logical_event(
        &self,
        logical_id: &str,
        rule_id: &str,
        event_id: &str,
        source_id: &str,
        event_revision: u64,
        now_ms: i64,
    ) -> anyhow::Result<EnqueueOutcome> {
        self.enqueue_logical_event_with_coalesced(
            logical_id,
            rule_id,
            event_id,
            source_id,
            event_revision,
            now_ms,
            0,
        )
    }

    /// Persists one alias and pending slot while recording `coalesced_replacements` from staging.
    #[allow(
        clippy::too_many_arguments,
        reason = "event identity fields map directly to the durable alias and pending intent schema"
    )]
    pub fn enqueue_logical_event_with_coalesced(
        &self,
        logical_id: &str,
        rule_id: &str,
        event_id: &str,
        source_id: &str,
        event_revision: u64,
        now_ms: i64,
        coalesced_replacements: u32,
    ) -> anyhow::Result<EnqueueOutcome> {
        let mut guard = self.lock_inner();
        let mut candidate = guard.runtime.clone();
        let effective_now = candidate.pacing.clamp_now(now_ms);
        if coalesced_replacements != 0 {
            candidate
                .pacing
                .record_suppression(effective_now, coalesced_replacements);
        }
        let existing_alias =
            pollster::block_on(load_alias_by_logical(&guard.connection, logical_id))?;
        let alias = build_alias(
            existing_alias.as_ref(),
            logical_id,
            event_id,
            source_id,
            event_revision,
            effective_now,
        );
        let outcome = apply_pending_enqueue(
            &mut candidate,
            alias.cloud_event_id,
            logical_id,
            rule_id,
            event_revision,
            effective_now,
        );
        pollster::block_on(persist_alias_and_runtime(
            &guard.connection,
            &alias,
            &candidate,
            effective_now,
            &guard.commit_fault,
        ))?;
        guard.runtime = candidate;
        Ok(outcome)
    }

    /// Atomically claims the next due attempt before any network I/O begins.
    ///
    /// Returns `None` when disabled, unpaired, throttled, backing off, or nothing is due.
    pub fn claim_next_attempt(
        &self,
        enabled: bool,
        signing: Option<SigningMaterial<'_>>,
        raw_now_ms: i64,
    ) -> anyhow::Result<Option<ClaimedAttempt>> {
        let Some(signing) = signing else {
            return Ok(None);
        };
        if !enabled {
            return Ok(None);
        }
        let mut guard = self.lock_inner();
        let mut candidate = guard.runtime.clone();
        let now_ms = candidate.pacing.clamp_now(raw_now_ms);
        if candidate.pacing.credential_rejected {
            return Ok(None);
        }
        let claimed = claim_existing_or_pending(&mut candidate, signing, now_ms)?;
        if candidate != guard.runtime {
            pollster::block_on(save_runtime_tx(
                &guard.connection,
                &candidate,
                &guard.commit_fault,
            ))?;
            guard.runtime = candidate;
        }
        Ok(claimed)
    }

    /// Records the outcome of a previously claimed attempt and updates retry or pacing state.
    pub fn finish_attempt(
        &self,
        cloud_event_id: [u8; 16],
        attempt_ordinal: u32,
        outcome: SignalDeliveryOutcome,
        still_enabled: bool,
        raw_now_ms: i64,
        jitter_unit_sample: f64,
    ) -> anyhow::Result<FinishOutcome> {
        let mut guard = self.lock_inner();
        let mut candidate = guard.runtime.clone();
        let now_ms = candidate.pacing.clamp_now(raw_now_ms);
        let Some(active) = candidate.claimed.as_mut() else {
            return Ok(FinishOutcome {
                terminal: true,
                accepted: false,
                failure: CloudFailureCode::None,
                next_attempt_at_ms: None,
            });
        };
        if active.cloud_event_id != cloud_event_id || active.attempt_count != attempt_ordinal {
            return Ok(FinishOutcome {
                terminal: false,
                accepted: false,
                failure: CloudFailureCode::None,
                next_attempt_at_ms: None,
            });
        }
        let result = apply_delivery_outcome(
            &mut candidate,
            outcome,
            still_enabled,
            now_ms,
            jitter_unit_sample,
        );
        pollster::block_on(save_runtime_tx(
            &guard.connection,
            &candidate,
            &guard.commit_fault,
        ))?;
        guard.runtime = candidate;
        Ok(result)
    }

    /// Reserves one test signal slot under the separate 3/hour test ceiling.
    ///
    /// Returns the pacing decision paired with the monotonically clamped clock timestamp.
    pub fn reserve_test_signal(&self, raw_now_ms: i64) -> anyhow::Result<(PaceDecision, i64)> {
        let mut guard = self.lock_inner();
        let mut candidate = guard.runtime.clone();
        let effective_now = candidate.pacing.clamp_now(raw_now_ms);
        let decision = candidate.pacing.try_consume_test_signal(effective_now);
        if decision == PaceDecision::Allowed {
            pollster::block_on(save_runtime_tx(
                &guard.connection,
                &candidate,
                &guard.commit_fault,
            ))?;
            guard.runtime = candidate;
        }
        Ok((decision, effective_now))
    }

    /// Applies a test signal outcome to health flags without altering production counters.
    pub fn record_test_outcome(
        &self,
        outcome: SignalDeliveryOutcome,
        raw_now_ms: i64,
    ) -> anyhow::Result<CloudFailureCode> {
        let mut guard = self.lock_inner();
        let mut candidate = guard.runtime.clone();
        let now_ms = candidate.pacing.clamp_now(raw_now_ms);
        let failure = match outcome {
            SignalDeliveryOutcome::Accepted => {
                candidate.pacing.last_failure = CloudFailureCode::None;
                CloudFailureCode::None
            }
            SignalDeliveryOutcome::BadRequest => {
                candidate.pacing.last_failure = CloudFailureCode::Dependency;
                CloudFailureCode::Dependency
            }
            SignalDeliveryOutcome::CredentialRejected => {
                candidate.pacing.credential_rejected = true;
                candidate.pacing.last_failure = CloudFailureCode::CredentialRejected;
                CloudFailureCode::CredentialRejected
            }
            SignalDeliveryOutcome::IntegrityConflict => {
                candidate.pacing.last_failure = CloudFailureCode::Integrity;
                CloudFailureCode::Integrity
            }
            SignalDeliveryOutcome::RateLimited { retry_after_ms } => {
                if let Some(delay) = retry_after_ms {
                    candidate.pacing.service_retry_after_until_ms =
                        Some(now_ms.saturating_add(i64::try_from(delay).unwrap_or(i64::MAX)));
                }
                candidate.pacing.last_failure = CloudFailureCode::ServiceLimit;
                CloudFailureCode::ServiceLimit
            }
            SignalDeliveryOutcome::ServiceUnavailable { retry_after_ms } => {
                if let Some(delay) = retry_after_ms {
                    candidate.pacing.backoff_until_ms =
                        Some(now_ms.saturating_add(i64::try_from(delay).unwrap_or(i64::MAX)));
                }
                candidate.pacing.last_failure = CloudFailureCode::Dependency;
                CloudFailureCode::Dependency
            }
            SignalDeliveryOutcome::DependencyFailure => {
                candidate.pacing.last_failure = CloudFailureCode::Dependency;
                CloudFailureCode::Dependency
            }
        };
        pollster::block_on(save_runtime_tx(
            &guard.connection,
            &candidate,
            &guard.commit_fault,
        ))?;
        guard.runtime = candidate;
        Ok(failure)
    }

    /// Cancels pending and claimed intents when disabled without erasing spent capacity.
    pub fn cancel_pending_on_disable(&self, raw_now_ms: i64) -> anyhow::Result<()> {
        let mut guard = self.lock_inner();
        let mut candidate = guard.runtime.clone();
        let _ = candidate.pacing.clamp_now(raw_now_ms);
        candidate.pending = None;
        candidate.claimed = None;
        pollster::block_on(save_runtime_tx(
            &guard.connection,
            &candidate,
            &guard.commit_fault,
        ))?;
        guard.runtime = candidate;
        Ok(())
    }

    /// Clears the `credential_rejected` latch after pairing credentials are repaired.
    pub fn mark_pairing_repaired(&self, raw_now_ms: i64) -> anyhow::Result<()> {
        let mut guard = self.lock_inner();
        let mut candidate = guard.runtime.clone();
        let _ = candidate.pacing.clamp_now(raw_now_ms);
        clear_credential_rejected(&mut candidate);
        pollster::block_on(save_runtime_tx(
            &guard.connection,
            &candidate,
            &guard.commit_fault,
        ))?;
        guard.runtime = candidate;
        Ok(())
    }

    /// Reconciles non-secret pairing metadata on startup or settings change.
    ///
    /// Clears the `credential_rejected` latch when pairing metadata changes or becomes unpaired.
    pub fn reconcile_pairing(
        &self,
        pairing: Option<PairingIdentity>,
        raw_now_ms: i64,
    ) -> anyhow::Result<bool> {
        let mut guard = self.lock_inner();
        if guard.runtime.pairing_identity == pairing {
            return Ok(false);
        }
        let mut candidate = guard.runtime.clone();
        let _ = candidate.pacing.clamp_now(raw_now_ms);
        let cleared = candidate.pacing.credential_rejected
            || candidate.pacing.last_failure == CloudFailureCode::CredentialRejected;
        candidate.pairing_identity = pairing;
        clear_credential_rejected(&mut candidate);
        pollster::block_on(save_runtime_tx(
            &guard.connection,
            &candidate,
            &guard.commit_fault,
        ))?;
        guard.runtime = candidate;
        Ok(cleared)
    }

    /// Resolves one opaque 16-byte `cloud_event_id` if present and unexpired.
    pub fn resolve_alias(
        &self,
        cloud_event_id: &[u8; 16],
        raw_now_ms: i64,
    ) -> anyhow::Result<Option<CloudEventAlias>> {
        let guard = self.lock_inner();
        let effective_now = guard.runtime.pacing.effective_now(raw_now_ms);
        let alias = pollster::block_on(load_alias_by_cloud_id(&guard.connection, cloud_event_id))?;
        Ok(alias.filter(|entry| entry.expires_at_ms > effective_now))
    }

    /// Returns a bounded health and suppression snapshot without exposing secrets or IDs.
    pub fn status_snapshot(
        &self,
        enabled: bool,
        paired: bool,
        raw_now_ms: i64,
    ) -> CloudStatusSnapshot {
        let guard = self.lock_inner();
        let pacing = &guard.runtime.pacing;
        let (accepted_last_hour, accepted_last_day) = pacing.accepted_counts(raw_now_ms);
        let (suppressed_last_hour, suppressed_last_day) = pacing.suppressed_counts(raw_now_ms);
        let pending = guard.runtime.pending.is_some() || guard.runtime.claimed.is_some();
        let state = pacing.derive_state(enabled, paired, raw_now_ms);
        let next_eligible_at_ms = if enabled && paired {
            pacing.next_eligible_at_ms(raw_now_ms)
        } else {
            None
        };
        CloudStatusSnapshot {
            state,
            paired,
            pending,
            accepted_last_hour,
            accepted_last_day,
            suppressed_last_hour,
            suppressed_last_day,
            next_eligible_at_ms,
            last_accepted_at_ms: pacing.last_accepted_at_ms,
            last_failure: pacing.last_failure,
        }
    }

    /// Returns whether a claim could succeed now without mutating durable state or doing disk I/O.
    pub fn can_claim_now(&self, enabled: bool, paired: bool, raw_now_ms: i64) -> bool {
        if !enabled || !paired {
            return false;
        }
        let guard = self.lock_inner();
        let runtime = &guard.runtime;
        if runtime.pacing.credential_rejected {
            return false;
        }
        let now_ms = runtime.pacing.effective_now(raw_now_ms);
        if let Some(claimed) = runtime.claimed.as_ref() {
            if claimed.in_flight {
                return claimed
                    .claimed_lease_until_ms
                    .is_some_and(|until| until <= now_ms);
            }
            return claimed.next_attempt_at_ms <= now_ms;
        }
        runtime.pacing.next_eligible_at_ms(raw_now_ms).is_none()
    }

    /// Returns the number of active pending/claimed intents (`0`, `1`, or `2` max).
    pub fn queue_depth(&self) -> (usize, usize) {
        let guard = self.lock_inner();
        (
            usize::from(guard.runtime.pending.is_some()),
            usize::from(guard.runtime.claimed.is_some()),
        )
    }

    fn lock_inner(&self) -> std::sync::MutexGuard<'_, DurableInner> {
        self.inner
            .lock()
            .expect("cloud notification state mutex must not be poisoned")
    }
}

fn recover_in_flight_on_startup(runtime: &mut PersistedCloudRuntime, now_ms: i64) {
    if let Some(claimed) = runtime.claimed.as_mut() {
        // Process restart guarantees no previous in-process worker holds this lease.
        claimed.in_flight = false;
        claimed.claimed_lease_until_ms = None;
        if claimed.expires_at_ms <= now_ms {
            runtime.pacing.last_failure = CloudFailureCode::Expired;
            runtime.pacing.record_suppression(now_ms, 1);
            runtime.claimed = None;
        } else if claimed.attempt_count >= MAX_ATTEMPTS_PER_INTENT {
            runtime.pacing.last_failure = CloudFailureCode::Dependency;
            runtime.pacing.record_suppression(now_ms, 1);
            runtime.claimed = None;
        }
    }
    if runtime
        .pending
        .as_ref()
        .is_some_and(|pending| pending.expires_at_ms <= now_ms)
    {
        runtime.pacing.last_failure = CloudFailureCode::Expired;
        runtime.pacing.record_suppression(now_ms, 1);
        runtime.pending = None;
    }
}

fn claim_existing_or_pending(
    runtime: &mut PersistedCloudRuntime,
    signing: SigningMaterial<'_>,
    now_ms: i64,
) -> anyhow::Result<Option<ClaimedAttempt>> {
    if let Some(attempt) = try_claim_existing(runtime, now_ms) {
        return Ok(Some(attempt));
    }
    if runtime.claimed.is_some() {
        return Ok(None);
    }
    try_claim_pending(runtime, signing, now_ms)
}

fn try_claim_existing(runtime: &mut PersistedCloudRuntime, now_ms: i64) -> Option<ClaimedAttempt> {
    let active = runtime.claimed.as_ref()?;
    if active.in_flight
        && active
            .claimed_lease_until_ms
            .is_some_and(|lease| lease > now_ms)
    {
        return None;
    }
    if active.expires_at_ms <= now_ms {
        runtime.pacing.last_failure = CloudFailureCode::Expired;
        runtime.pacing.record_suppression(now_ms, 1);
        runtime.claimed = None;
        return None;
    }
    if active.attempt_count >= MAX_ATTEMPTS_PER_INTENT {
        runtime.pacing.last_failure = CloudFailureCode::Dependency;
        runtime.pacing.record_suppression(now_ms, 1);
        runtime.claimed = None;
        return None;
    }
    if active.next_attempt_at_ms > now_ms {
        return None;
    }
    if let PaceDecision::Delayed {
        next_eligible_at_ms,
        ..
    } = runtime.pacing.check_production_attempt(now_ms, true)
    {
        if let Some(claimed) = runtime.claimed.as_mut() {
            claimed.next_attempt_at_ms = claimed.next_attempt_at_ms.max(next_eligible_at_ms);
        }
        return None;
    }
    runtime.pacing.consume_gcra_attempt(now_ms);
    let claimed = runtime
        .claimed
        .as_mut()
        .expect("claimed intent must exist while re-claiming");
    claimed.attempt_count = claimed
        .attempt_count
        .checked_add(1)
        .expect("bounded attempt count must not overflow");
    claimed.in_flight = true;
    claimed.claimed_lease_until_ms = Some(now_ms.saturating_add(CLAIM_LEASE_MS));
    Some(ClaimedAttempt {
        cloud_event_id: claimed.cloud_event_id,
        logical_id: claimed.logical_id.clone(),
        rule_id: claimed.rule_id.clone(),
        revision: claimed.revision,
        attempt_ordinal: claimed.attempt_count,
        canonical_body: claimed.canonical_body,
        expires_at_ms: claimed.expires_at_ms,
    })
}

fn try_claim_pending(
    runtime: &mut PersistedCloudRuntime,
    signing: SigningMaterial<'_>,
    now_ms: i64,
) -> anyhow::Result<Option<ClaimedAttempt>> {
    let Some(pending) = runtime.pending.as_ref() else {
        return Ok(None);
    };
    if pending.expires_at_ms <= now_ms {
        runtime.pacing.last_failure = CloudFailureCode::Expired;
        runtime.pacing.record_suppression(now_ms, 1);
        runtime.pending = None;
        return Ok(None);
    }
    if let PaceDecision::Delayed { .. } = runtime.pacing.check_production_attempt(now_ms, false) {
        return Ok(None);
    }
    let pending = runtime
        .pending
        .take()
        .expect("pending intent must exist after check");
    let issued_at_secs =
        u64::try_from((now_ms.max(0)) / 1_000).expect("non-negative Unix seconds must fit in u64");
    let canonical_body = sign_signal(
        SignalEnvelope {
            nvr_id: signing.nvr_id,
            cloud_event_id: pending.cloud_event_id,
            issued_at_secs,
            signing_key_version: signing.signing_key_version,
        },
        signing.seed,
    )?;
    runtime.pacing.consume_gcra_attempt(now_ms);
    let claimed = ClaimedCloudIntent {
        cloud_event_id: pending.cloud_event_id,
        logical_id: pending.logical_id.clone(),
        rule_id: pending.rule_id.clone(),
        revision: pending.revision,
        canonical_body,
        signing_key_version: signing.signing_key_version,
        nvr_id: signing.nvr_id,
        attempt_count: 1,
        created_at_ms: now_ms,
        expires_at_ms: pending.expires_at_ms,
        next_attempt_at_ms: now_ms,
        claimed_lease_until_ms: Some(now_ms.saturating_add(CLAIM_LEASE_MS)),
        in_flight: true,
    };
    let attempt = ClaimedAttempt {
        cloud_event_id: claimed.cloud_event_id,
        logical_id: claimed.logical_id.clone(),
        rule_id: claimed.rule_id.clone(),
        revision: claimed.revision,
        attempt_ordinal: 1,
        canonical_body,
        expires_at_ms: claimed.expires_at_ms,
    };
    runtime.claimed = Some(claimed);
    Ok(Some(attempt))
}

fn clear_credential_rejected(runtime: &mut PersistedCloudRuntime) {
    runtime.pacing.credential_rejected = false;
    if runtime.pacing.last_failure == CloudFailureCode::CredentialRejected {
        runtime.pacing.last_failure = CloudFailureCode::None;
    }
}

fn build_alias(
    existing_alias: Option<&CloudEventAlias>,
    logical_id: &str,
    event_id: &str,
    source_id: &str,
    event_revision: u64,
    effective_now: i64,
) -> CloudEventAlias {
    let cloud_event_id =
        existing_alias.map_or_else(|| *Uuid::new_v4().as_bytes(), |alias| alias.cloud_event_id);
    CloudEventAlias {
        cloud_event_id,
        logical_id: logical_id.to_owned(),
        event_id: event_id.to_owned(),
        source_id: source_id.to_owned(),
        event_revision,
        created_at_ms: existing_alias.map_or(effective_now, |entry| entry.created_at_ms),
        expires_at_ms: effective_now.saturating_add(ALIAS_TTL_MS),
    }
}

fn apply_pending_enqueue(
    candidate: &mut PersistedCloudRuntime,
    cloud_event_id: [u8; 16],
    logical_id: &str,
    rule_id: &str,
    event_revision: u64,
    effective_now: i64,
) -> EnqueueOutcome {
    if candidate.accepted_ids.contains(&cloud_event_id) {
        return EnqueueOutcome {
            cloud_event_id,
            queued: false,
            replaced_previous: false,
            already_accepted: true,
        };
    }
    let replaced_previous = candidate
        .pending
        .as_ref()
        .is_some_and(|pending| pending.cloud_event_id != cloud_event_id);
    if replaced_previous {
        candidate.pacing.record_suppression(effective_now, 1);
    }
    candidate.pending = Some(PendingCloudIntent {
        cloud_event_id,
        logical_id: logical_id.to_owned(),
        rule_id: rule_id.to_owned(),
        revision: event_revision,
        enqueued_at_ms: effective_now,
        expires_at_ms: effective_now.saturating_add(INTENT_TTL_MS),
    });
    EnqueueOutcome {
        cloud_event_id,
        queued: true,
        replaced_previous,
        already_accepted: false,
    }
}

fn apply_delivery_outcome(
    runtime: &mut PersistedCloudRuntime,
    outcome: SignalDeliveryOutcome,
    still_enabled: bool,
    now_ms: i64,
    jitter_unit_sample: f64,
) -> FinishOutcome {
    let active = runtime
        .claimed
        .as_ref()
        .expect("claimed intent must exist when finishing attempt")
        .clone();
    match outcome {
        SignalDeliveryOutcome::Accepted => finish_accepted(runtime, active.cloud_event_id, now_ms),
        SignalDeliveryOutcome::BadRequest => {
            finish_terminal_failure(runtime, CloudFailureCode::Dependency, false, now_ms)
        }
        SignalDeliveryOutcome::CredentialRejected => {
            finish_terminal_failure(runtime, CloudFailureCode::CredentialRejected, true, now_ms)
        }
        SignalDeliveryOutcome::IntegrityConflict => {
            finish_terminal_failure(runtime, CloudFailureCode::Integrity, false, now_ms)
        }
        SignalDeliveryOutcome::RateLimited { retry_after_ms } => schedule_retry_or_terminate(
            runtime,
            &active,
            CloudFailureCode::ServiceLimit,
            retry_after_ms,
            true,
            still_enabled,
            now_ms,
            jitter_unit_sample,
        ),
        SignalDeliveryOutcome::ServiceUnavailable { retry_after_ms } => {
            schedule_retry_or_terminate(
                runtime,
                &active,
                CloudFailureCode::Dependency,
                retry_after_ms,
                false,
                still_enabled,
                now_ms,
                jitter_unit_sample,
            )
        }
        SignalDeliveryOutcome::DependencyFailure => schedule_retry_or_terminate(
            runtime,
            &active,
            CloudFailureCode::Dependency,
            None,
            false,
            still_enabled,
            now_ms,
            jitter_unit_sample,
        ),
    }
}

fn finish_accepted(
    runtime: &mut PersistedCloudRuntime,
    cloud_event_id: [u8; 16],
    now_ms: i64,
) -> FinishOutcome {
    runtime.pacing.record_production_accepted(now_ms);
    runtime.accepted_ids.push_back(cloud_event_id);
    while runtime.accepted_ids.len() > NVR_DAILY_LIMIT {
        runtime.accepted_ids.pop_front();
    }
    runtime.claimed = None;
    FinishOutcome {
        terminal: true,
        accepted: true,
        failure: CloudFailureCode::None,
        next_attempt_at_ms: None,
    }
}

fn finish_terminal_failure(
    runtime: &mut PersistedCloudRuntime,
    failure: CloudFailureCode,
    latch_credential: bool,
    now_ms: i64,
) -> FinishOutcome {
    if latch_credential {
        runtime.pacing.credential_rejected = true;
    }
    runtime.pacing.last_failure = failure;
    runtime.pacing.record_suppression(now_ms, 1);
    runtime.claimed = None;
    FinishOutcome {
        terminal: true,
        accepted: false,
        failure,
        next_attempt_at_ms: None,
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "retry scheduling combines active intent bounds, failure code, retry-after, and clock"
)]
fn schedule_retry_or_terminate(
    runtime: &mut PersistedCloudRuntime,
    active: &ClaimedCloudIntent,
    failure: CloudFailureCode,
    retry_after_ms: Option<u64>,
    is_service_limit: bool,
    still_enabled: bool,
    now_ms: i64,
    jitter_unit_sample: f64,
) -> FinishOutcome {
    if is_service_limit && let Some(delay) = retry_after_ms {
        let until = now_ms.saturating_add(i64::try_from(delay).unwrap_or(i64::MAX));
        runtime.pacing.service_retry_after_until_ms = Some(until);
    }
    if !still_enabled {
        runtime.pacing.last_failure = failure;
        runtime.claimed = None;
        return FinishOutcome {
            terminal: true,
            accepted: false,
            failure,
            next_attempt_at_ms: None,
        };
    }
    let next_at = compute_next_retry_at_ms(
        now_ms,
        active.attempt_count,
        active.expires_at_ms,
        retry_after_ms,
        jitter_unit_sample,
    );
    match next_at {
        Some(next_attempt_at_ms) => {
            record_scheduled_retry(runtime, failure, is_service_limit, next_attempt_at_ms)
        }
        None => {
            let terminal_code = if now_ms >= active.expires_at_ms
                || active.attempt_count < MAX_ATTEMPTS_PER_INTENT
            {
                CloudFailureCode::Expired
            } else {
                failure
            };
            finish_terminal_failure(runtime, terminal_code, false, now_ms)
        }
    }
}

fn record_scheduled_retry(
    runtime: &mut PersistedCloudRuntime,
    failure: CloudFailureCode,
    is_service_limit: bool,
    next_attempt_at_ms: i64,
) -> FinishOutcome {
    runtime.pacing.last_failure = failure;
    if is_service_limit {
        runtime.pacing.service_retry_after_until_ms = Some(
            runtime
                .pacing
                .service_retry_after_until_ms
                .map_or(next_attempt_at_ms, |current| {
                    current.max(next_attempt_at_ms)
                }),
        );
    } else {
        runtime.pacing.backoff_until_ms = Some(next_attempt_at_ms);
    }
    if let Some(claimed) = runtime.claimed.as_mut() {
        claimed.in_flight = false;
        claimed.claimed_lease_until_ms = None;
        claimed.next_attempt_at_ms = next_attempt_at_ms;
    }
    FinishOutcome {
        terminal: false,
        accepted: false,
        failure,
        next_attempt_at_ms: Some(next_attempt_at_ms),
    }
}

async fn initialize_schema(connection: &turso::Connection) -> anyhow::Result<()> {
    for pragma in ["PRAGMA journal_mode = WAL", "PRAGMA synchronous = FULL"] {
        let mut rows = connection.query(pragma, ()).await?;
        while rows.next().await?.is_some() {}
    }
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS cloud_runtime (
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 payload_json TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS cloud_aliases (
                 cloud_event_id BLOB PRIMARY KEY,
                 logical_id TEXT NOT NULL UNIQUE,
                 event_id TEXT NOT NULL,
                 source_id TEXT NOT NULL,
                 event_revision INTEGER NOT NULL,
                 created_at_ms INTEGER NOT NULL,
                 expires_at_ms INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_cloud_aliases_expires
                 ON cloud_aliases (expires_at_ms);",
        )
        .await?;
    Ok(())
}

async fn load_runtime(connection: &turso::Connection) -> anyhow::Result<PersistedCloudRuntime> {
    let mut rows = connection
        .query("SELECT payload_json FROM cloud_runtime WHERE id = 1", ())
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(PersistedCloudRuntime::fresh());
    };
    let json: String = row.get(0)?;
    let parsed: PersistedCloudRuntime = serde_json::from_str(&json)?;
    anyhow::ensure!(
        parsed.schema_version == CLOUD_STATE_SCHEMA_VERSION,
        "unsupported cloud notification state schema version {}",
        parsed.schema_version
    );
    Ok(parsed)
}

async fn save_runtime_tx(
    connection: &turso::Connection,
    runtime: &PersistedCloudRuntime,
    commit_fault: &CommitFaultHook,
) -> anyhow::Result<()> {
    let json = serde_json::to_string(runtime)?;
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let write = async {
        connection
            .execute(
                "INSERT INTO cloud_runtime(id, payload_json) VALUES (1, ?1)
                 ON CONFLICT(id) DO UPDATE SET payload_json = excluded.payload_json",
                turso::params![json],
            )
            .await?;
        if commit_fault.take() {
            anyhow::bail!("injected cloud state commit failure");
        }
        Ok(())
    }
    .await;
    finish_tx(connection, write).await
}

async fn persist_alias_and_runtime(
    connection: &turso::Connection,
    alias: &CloudEventAlias,
    runtime: &PersistedCloudRuntime,
    now_ms: i64,
    commit_fault: &CommitFaultHook,
) -> anyhow::Result<()> {
    let json = serde_json::to_string(runtime)?;
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let write = async {
        connection
            .execute(
                "INSERT INTO cloud_aliases(
                     cloud_event_id, logical_id, event_id, source_id,
                     event_revision, created_at_ms, expires_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(cloud_event_id) DO UPDATE SET
                     event_id = excluded.event_id,
                     source_id = excluded.source_id,
                     event_revision = excluded.event_revision,
                     expires_at_ms = excluded.expires_at_ms",
                turso::params![
                    alias.cloud_event_id.to_vec(),
                    alias.logical_id.as_str(),
                    alias.event_id.as_str(),
                    alias.source_id.as_str(),
                    i64::try_from(alias.event_revision)?,
                    alias.created_at_ms,
                    alias.expires_at_ms
                ],
            )
            .await?;
        connection
            .execute(
                "DELETE FROM cloud_aliases WHERE expires_at_ms <= ?1",
                turso::params![now_ms],
            )
            .await?;
        connection
            .execute(
                "DELETE FROM cloud_aliases
                 WHERE cloud_event_id IN (
                     SELECT cloud_event_id FROM cloud_aliases
                     ORDER BY created_at_ms DESC, cloud_event_id DESC
                     LIMIT -1 OFFSET ?1
                 )",
                turso::params![i64::try_from(MAX_CLOUD_ALIASES)?],
            )
            .await?;
        connection
            .execute(
                "INSERT INTO cloud_runtime(id, payload_json) VALUES (1, ?1)
                 ON CONFLICT(id) DO UPDATE SET payload_json = excluded.payload_json",
                turso::params![json],
            )
            .await?;
        if commit_fault.take() {
            anyhow::bail!("injected cloud state commit failure");
        }
        Ok(())
    }
    .await;
    finish_tx(connection, write).await
}

async fn prune_aliases_tx(connection: &turso::Connection, now_ms: i64) -> anyhow::Result<()> {
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let write = async {
        connection
            .execute(
                "DELETE FROM cloud_aliases WHERE expires_at_ms <= ?1",
                turso::params![now_ms],
            )
            .await?;
        Ok(())
    }
    .await;
    finish_tx(connection, write).await
}

async fn load_alias_by_logical(
    connection: &turso::Connection,
    logical_id: &str,
) -> anyhow::Result<Option<CloudEventAlias>> {
    let mut rows = connection
        .query(
            "SELECT cloud_event_id, logical_id, event_id, source_id,
                    event_revision, created_at_ms, expires_at_ms
             FROM cloud_aliases WHERE logical_id = ?1",
            turso::params![logical_id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    parse_alias_row(&row).map(Some)
}

async fn load_alias_by_cloud_id(
    connection: &turso::Connection,
    cloud_event_id: &[u8; 16],
) -> anyhow::Result<Option<CloudEventAlias>> {
    let mut rows = connection
        .query(
            "SELECT cloud_event_id, logical_id, event_id, source_id,
                    event_revision, created_at_ms, expires_at_ms
             FROM cloud_aliases WHERE cloud_event_id = ?1",
            turso::params![cloud_event_id.to_vec()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    parse_alias_row(&row).map(Some)
}

fn parse_alias_row(row: &turso::Row) -> anyhow::Result<CloudEventAlias> {
    let raw_id: Vec<u8> = row.get(0)?;
    let cloud_event_id: [u8; 16] = raw_id
        .try_into()
        .map_err(|_| anyhow::anyhow!("stored cloud_event_id must be 16 bytes"))?;
    Ok(CloudEventAlias {
        cloud_event_id,
        logical_id: row.get(1)?,
        event_id: row.get(2)?,
        source_id: row.get(3)?,
        event_revision: u64::try_from(row.get::<i64>(4)?)?,
        created_at_ms: row.get(5)?,
        expires_at_ms: row.get(6)?,
    })
}

async fn finish_tx(
    connection: &turso::Connection,
    result: anyhow::Result<()>,
) -> anyhow::Result<()> {
    match result {
        Ok(()) => {
            if let Err(error) = connection.execute_batch("COMMIT").await {
                let _ = connection.execute_batch("ROLLBACK").await;
                return Err(error.into());
            }
            Ok(())
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK").await;
            Err(error)
        }
    }
}

mod canonical_body_serde {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use serde::{Deserializer, Serializer, de};

    use super::CANONICAL_BODY_BYTES;

    pub fn serialize<S>(
        bytes: &[u8; CANONICAL_BODY_BYTES],
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<[u8; CANONICAL_BODY_BYTES], D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded: String = serde::Deserialize::deserialize(deserializer)?;
        let decoded = STANDARD.decode(encoded).map_err(de::Error::custom)?;
        decoded
            .try_into()
            .map_err(|_| de::Error::custom("canonical body must be exactly 109 bytes"))
    }
}
