use std::{
    collections::{HashMap, HashSet},
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
    recording_outages: HashMap<String, String>,
    storage_outage: Option<String>,
}

impl HealthState {
    fn observe(&mut self, snapshot: &RecordingHealthSnapshot, now_ms: i64) -> Vec<Candidate> {
        let failed = snapshot
            .streams
            .iter()
            .filter(|stream| stream.last_error.is_some())
            .map(|stream| stream.stream_id.clone())
            .collect::<HashSet<_>>();
        let mut candidates = Vec::with_capacity(failed.len().saturating_add(2));
        for stream in &snapshot.streams {
            if stream.last_error.is_none() || self.recording_outages.contains_key(&stream.stream_id)
            {
                continue;
            }
            let identity = format!("recording-outage-{}", uuid::Uuid::new_v4());
            self.recording_outages
                .insert(stream.stream_id.clone(), identity.clone());
            candidates.push(health_candidate(
                Trigger::RecordingHealth,
                &stream.stream_id,
                identity,
                Lifecycle::Recording,
                stream
                    .last_failure_at_ms
                    .and_then(|value| i64::try_from(value).ok())
                    .unwrap_or(now_ms),
            ));
        }
        let recovered = self
            .recording_outages
            .keys()
            .filter(|stream_id| !failed.contains(*stream_id))
            .cloned()
            .collect::<Vec<_>>();
        for stream_id in recovered {
            let identity = self
                .recording_outages
                .remove(&stream_id)
                .expect("recovered recording outage must exist");
            candidates.push(health_candidate(
                Trigger::Recovery,
                &stream_id,
                identity,
                Lifecycle::Recording,
                now_ms,
            ));
        }

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
        group_ids: Vec::new(),
        zone: None,
        confidence: None,
        attachment_path: None,
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
    use crate::storage::{RecordingStreamHealthSnapshot, safety::StorageSafetyHealthSnapshot};

    use super::*;

    fn stream(error: Option<&str>, failure_at_ms: Option<u64>) -> RecordingStreamHealthSnapshot {
        RecordingStreamHealthSnapshot {
            stream_id: "front-door/sub".to_owned(),
            last_attempt_at_ms: Some(1_000),
            attempt_age_ms: Some(500),
            last_progress_at_ms: Some(500),
            progress_age_ms: Some(1_000),
            last_failure_at_ms: failure_at_ms,
            failure_age_ms: failure_at_ms.map(|_| 500),
            last_error: error.map(str::to_owned),
            recorded_duration_ms: 0,
        }
    }

    #[test]
    fn recording_failure_and_recovery_emit_one_interval_each() {
        let mut state = HealthState::default();
        let failed = RecordingHealthSnapshot {
            streams: vec![stream(Some("disk full"), Some(1_000))],
            storage: StorageSafetyHealthSnapshot {
                pressure: StoragePressure::Critical,
                recording_state: StorageRecordingState::Paused,
                last_failure_at_ms: Some(1_200),
                ..StorageSafetyHealthSnapshot::default()
            },
        };
        let started = state.observe(&failed, 1_500);
        assert_eq!(started.len(), 2);
        assert!(
            started
                .iter()
                .any(|candidate| candidate.trigger == Trigger::RecordingHealth)
        );
        assert!(
            started
                .iter()
                .any(|candidate| candidate.trigger == Trigger::StorageHealth)
        );
        assert!(state.observe(&failed, 2_000).is_empty());

        let recovered = state.observe(
            &RecordingHealthSnapshot {
                streams: vec![stream(None, None)],
                storage: StorageSafetyHealthSnapshot::default(),
            },
            3_000,
        );
        assert_eq!(recovered.len(), 2);
        assert!(
            recovered
                .iter()
                .all(|candidate| candidate.trigger == Trigger::Recovery)
        );
        assert!(
            recovered
                .iter()
                .all(|candidate| candidate.stage == Stage::Recovery)
        );
    }
}
