use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
    shutdown::Shutdown,
    storage::{
        health::{RecordingHealthRegistry, RecordingHealthSnapshot},
        safety::{StoragePressure, StorageRecordingState},
    },
};

use super::{
    Handle, Lifecycle, Stage,
    model::{Candidate, Severity, Trigger},
};

const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(500);

pub struct HealthMonitor {
    cancel: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl HealthMonitor {
    pub fn start(
        health: RecordingHealthRegistry,
        notifications: Handle,
        shutdown: Shutdown,
    ) -> anyhow::Result<Self> {
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let thread = std::thread::Builder::new()
            .name("notification-health".to_owned())
            .spawn(move || {
                let mut state = HealthState::default();
                while !shutdown.is_cancelled() && !worker_cancel.load(Ordering::Acquire) {
                    for candidate in state.observe(&health.snapshot(), unix_time_ms()) {
                        notifications.publish(candidate);
                    }
                    std::thread::sleep(HEALTH_POLL_INTERVAL);
                }
            })?;
        Ok(Self {
            cancel,
            thread: Some(thread),
        })
    }

    pub fn join(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("notification health monitor panicked");
        }
    }
}

impl Drop for HealthMonitor {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Debug, Default)]
struct HealthState {
    storage_outage: Option<String>,
}

impl HealthState {
    fn observe(&mut self, snapshot: &RecordingHealthSnapshot, now_ms: i64) -> Vec<Candidate> {
        let mut candidates = Vec::with_capacity(1);
        let storage_failed = snapshot.storage.pressure == StoragePressure::Critical
            || snapshot.storage.recording_state == StorageRecordingState::Paused;
        if storage_failed && self.storage_outage.is_none() {
            let identity = format!("storage-outage-{}", uuid::Uuid::new_v4());
            self.storage_outage = Some(identity.clone());
            candidates.push(health_candidate(
                Trigger::StorageHealth,
                "storage",
                identity,
                Lifecycle::Storage,
                snapshot
                    .storage
                    .last_failure_at_ms
                    .or(snapshot.storage.last_evaluation_at_ms)
                    .and_then(|value| i64::try_from(value).ok())
                    .unwrap_or(now_ms),
            ));
        } else if !storage_failed && let Some(identity) = self.storage_outage.take() {
            candidates.push(health_candidate(
                Trigger::Recovery,
                "storage",
                identity,
                Lifecycle::Storage,
                now_ms,
            ));
        }
        candidates
    }
}

fn health_candidate(
    trigger: Trigger,
    source_id: &str,
    source_identity: String,
    lifecycle: Lifecycle,
    occurred_at_ms: i64,
) -> Candidate {
    let recovery = trigger == Trigger::Recovery;
    let event_kind = match lifecycle {
        Lifecycle::Recording => "recording_health",
        Lifecycle::Storage => "storage_health",
        Lifecycle::Event | Lifecycle::Outage | Lifecycle::Test => "health",
    };
    Candidate {
        trigger,
        source_id: source_id.to_owned(),
        source_name: None,
        source_identity,
        lifecycle,
        event_kind: Some(event_kind.to_owned()),
        payload: None,
        group_ids: Vec::new(),
        zone: None,
        confidence: None,
        attachment_path: None,
        canonical_attachment: None,
        icon_key: Some("alert".to_owned()),
        image_available: false,
        duration_ms: None,
        severity: if recovery {
            Severity::Info
        } else if lifecycle == Lifecycle::Storage {
            Severity::Critical
        } else {
            Severity::Warning
        },
        reviewed: None,
        bookmarked: None,
        privacy_active: false,
        revision: if recovery { 2 } else { 1 },
        stage: if recovery {
            Stage::Recovery
        } else {
            Stage::Preliminary
        },
        occurred_at_ms,
        deep_link: "/system-health".to_owned(),
    }
}

fn unix_time_ms() -> i64 {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(milliseconds).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use crate::storage::safety::StorageSafetyHealthSnapshot;

    use super::*;

    #[test]
    fn storage_failure_and_recovery_emit_one_interval_each() {
        let mut state = HealthState::default();
        let failed = RecordingHealthSnapshot {
            streams: Vec::new(),
            storage: StorageSafetyHealthSnapshot {
                pressure: StoragePressure::Critical,
                recording_state: StorageRecordingState::Paused,
                last_failure_at_ms: Some(1_200),
                ..StorageSafetyHealthSnapshot::default()
            },
        };
        let started = state.observe(&failed, 1_500);
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].trigger, Trigger::StorageHealth);
        assert!(state.observe(&failed, 2_000).is_empty());

        let recovered = state.observe(
            &RecordingHealthSnapshot {
                streams: Vec::new(),
                storage: StorageSafetyHealthSnapshot::default(),
            },
            3_000,
        );
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].trigger, Trigger::Recovery);
        assert_eq!(recovered[0].stage, Stage::Recovery);
    }
}
