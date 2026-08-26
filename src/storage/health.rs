use crate::storage::safety::{StorageSafetyHealthRegistry, StorageSafetyHealthSnapshot};
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};

const MAX_ERROR_CHARS: usize = 240;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordingHealthSnapshot {
    pub streams: Vec<RecordingStreamHealthSnapshot>,
    pub storage: StorageSafetyHealthSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordingStreamHealthSnapshot {
    pub stream_id: String,
    pub last_attempt_at_ms: Option<u64>,
    pub attempt_age_ms: Option<u64>,
    pub last_progress_at_ms: Option<u64>,
    pub progress_age_ms: Option<u64>,
    pub last_failure_at_ms: Option<u64>,
    pub failure_age_ms: Option<u64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Default)]
pub struct RecordingHealthRegistry {
    inner: Arc<Mutex<HashMap<String, RecordingStreamHealth>>>,
    storage: StorageSafetyHealthRegistry,
}

#[derive(Default)]
struct RecordingStreamHealth {
    last_attempt: Option<Observation>,
    pending_since: Option<Observation>,
    last_progress: Option<Observation>,
    last_failure: Option<Observation>,
    last_error: Option<String>,
}

#[derive(Clone, Copy)]
struct Observation {
    at: Instant,
    at_ms: u64,
}

impl RecordingHealthRegistry {
    pub(crate) fn note_attempt(&self, stream_id: &str) {
        self.note_attempt_at(stream_id, Instant::now(), unix_time_ms());
    }

    pub(crate) fn note_progress(&self, stream_id: &str) {
        self.note_progress_at(stream_id, Instant::now(), unix_time_ms());
    }

    pub(crate) fn note_failure(&self, stream_id: &str, error: &str) {
        self.note_failure_at(stream_id, error, Instant::now(), unix_time_ms());
    }

    pub(crate) fn snapshot(&self) -> RecordingHealthSnapshot {
        self.snapshot_at(Instant::now())
    }

    pub(crate) fn storage(&self) -> StorageSafetyHealthRegistry {
        self.storage.clone()
    }

    fn note_attempt_at(&self, stream_id: &str, at: Instant, at_ms: u64) {
        let mut streams = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stream = streams.entry(stream_id.to_owned()).or_default();
        let observation = Observation { at, at_ms };
        stream.last_attempt = Some(observation);
        stream.pending_since.get_or_insert(observation);
    }

    fn note_progress_at(&self, stream_id: &str, at: Instant, at_ms: u64) {
        let mut streams = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stream = streams.entry(stream_id.to_owned()).or_default();
        let observation = Observation { at, at_ms };
        stream.last_attempt = Some(observation);
        stream.pending_since = None;
        stream.last_progress = Some(observation);
        stream.last_failure = None;
        stream.last_error = None;
    }

    fn note_failure_at(&self, stream_id: &str, error: &str, at: Instant, at_ms: u64) {
        let mut streams = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stream = streams.entry(stream_id.to_owned()).or_default();
        let observation = Observation { at, at_ms };
        stream.last_attempt = Some(observation);
        stream.pending_since.get_or_insert(observation);
        stream.last_failure = Some(observation);
        stream.last_error = Some(error.chars().take(MAX_ERROR_CHARS).collect());
    }

    fn snapshot_at(&self, now: Instant) -> RecordingHealthSnapshot {
        let streams = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut snapshots = streams
            .iter()
            .map(|(stream_id, stream)| RecordingStreamHealthSnapshot {
                stream_id: stream_id.clone(),
                last_attempt_at_ms: stream.last_attempt.map(|observation| observation.at_ms),
                attempt_age_ms: observation_age_ms(stream.pending_since, now),
                last_progress_at_ms: stream.last_progress.map(|observation| observation.at_ms),
                progress_age_ms: observation_age_ms(stream.last_progress, now),
                last_failure_at_ms: stream.last_failure.map(|observation| observation.at_ms),
                failure_age_ms: observation_age_ms(stream.last_failure, now),
                last_error: stream.last_error.clone(),
            })
            .collect::<Vec<_>>();
        snapshots.sort_unstable_by(|left, right| left.stream_id.cmp(&right.stream_id));
        RecordingHealthSnapshot {
            streams: snapshots,
            storage: self.storage.snapshot(),
        }
    }
}

fn observation_age_ms(observation: Option<Observation>, now: Instant) -> Option<u64> {
    observation.map(|observation| {
        now.saturating_duration_since(observation.at)
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    })
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn new_progress_clears_obsolete_writer_failure() {
        let health = RecordingHealthRegistry::default();
        let started_at = Instant::now();
        health.note_attempt_at("front/sub", started_at, 1_000);
        health.note_progress_at("front/sub", started_at + Duration::from_secs(1), 2_000);
        health.note_failure_at(
            "front/sub",
            &"disk full ".repeat(40),
            started_at + Duration::from_secs(2),
            3_000,
        );

        let failed = health.snapshot_at(started_at + Duration::from_secs(3));
        assert_eq!(failed.streams[0].attempt_age_ms, Some(1_000));
        assert_eq!(failed.streams[0].progress_age_ms, Some(2_000));
        assert_eq!(failed.streams[0].failure_age_ms, Some(1_000));
        assert_eq!(
            failed.streams[0]
                .last_error
                .as_ref()
                .map(|error| error.chars().count()),
            Some(MAX_ERROR_CHARS)
        );

        health.note_progress_at("front/sub", started_at + Duration::from_secs(4), 5_000);
        let recovered = health.snapshot_at(started_at + Duration::from_secs(5));
        assert_eq!(recovered.streams[0].progress_age_ms, Some(1_000));
        assert_eq!(recovered.streams[0].attempt_age_ms, None);
        assert_eq!(recovered.streams[0].last_failure_at_ms, None);
        assert_eq!(recovered.streams[0].last_error, None);
    }

    #[test]
    fn repeated_attempts_do_not_postpone_stall_detection() {
        let health = RecordingHealthRegistry::default();
        let started_at = Instant::now();
        health.note_attempt_at("front/sub", started_at, 1_000);
        health.note_attempt_at("front/sub", started_at + Duration::from_secs(20), 21_000);

        let snapshot = health.snapshot_at(started_at + Duration::from_secs(30));
        assert_eq!(snapshot.streams[0].last_attempt_at_ms, Some(21_000));
        assert_eq!(snapshot.streams[0].attempt_age_ms, Some(30_000));
        assert_eq!(snapshot.streams[0].last_progress_at_ms, None);
    }
}
