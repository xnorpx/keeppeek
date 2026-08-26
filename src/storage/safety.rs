use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

const MAX_ERROR_CHARS: usize = 240;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageSafetyPolicy {
    pub archive_max_bytes: u64,
    pub minimum_free_bytes: u64,
    pub maximum_used_percent: Option<u8>,
    pub warning_free_bytes: u64,
    pub critical_free_bytes: u64,
    pub cleanup_hysteresis_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilesystemCapacity {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub keeppeek_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StoragePressure {
    Normal,
    Warning,
    Critical,
}

impl StoragePressure {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }

    pub const fn metric_value(self) -> u64 {
        match self {
            Self::Normal => 0,
            Self::Warning => 1,
            Self::Critical => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageRecordingState {
    Active,
    Degraded,
    Paused,
}

impl StorageRecordingState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Degraded => "degraded",
            Self::Paused => "paused",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageCleanupTrigger {
    Startup,
    SegmentFinalized,
    Periodic,
}

impl StorageCleanupTrigger {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::SegmentFinalized => "segment_finalized",
            Self::Periodic => "periodic",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageCleanupReason {
    ArchiveCap,
    FilesystemHeadroom,
    Combined,
    Reconciliation,
}

impl StorageCleanupReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ArchiveCap => "archive_cap",
            Self::FilesystemHeadroom => "filesystem_headroom",
            Self::Combined => "combined",
            Self::Reconciliation => "reconciliation",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageSafetyEvaluation {
    pub pressure: StoragePressure,
    pub cleanup_required: bool,
    pub effective_limit_bytes: Option<u64>,
    pub cleanup_target_bytes: Option<u64>,
    pub warning_free_bytes: u64,
    pub critical_free_bytes: u64,
    pub recovery_free_bytes: u64,
    pub cleanup_reason: Option<StorageCleanupReason>,
}

impl StorageSafetyPolicy {
    pub fn evaluate(self, capacity: FilesystemCapacity) -> StorageSafetyEvaluation {
        let critical_free_bytes = self.critical_free_bytes.max(self.minimum_free_bytes);
        let configured_warning_free_bytes =
            if self.warning_free_bytes == 0 && critical_free_bytes > 0 {
                critical_free_bytes.saturating_add(self.cleanup_hysteresis_bytes)
            } else {
                self.warning_free_bytes.max(critical_free_bytes)
            };
        let percent_free_bytes = self.maximum_used_percent.map_or(0, |maximum_used_percent| {
            capacity
                .total_bytes
                .saturating_sub(percent_of(capacity.total_bytes, maximum_used_percent))
        });
        let warning_free_bytes = configured_warning_free_bytes.max(percent_free_bytes);
        let has_free_space_limit = warning_free_bytes > 0;
        let available_bytes = capacity.available_bytes.min(capacity.total_bytes);
        let keeppeek_bytes = capacity.keeppeek_bytes.min(capacity.total_bytes);
        let other_used_bytes = capacity
            .total_bytes
            .saturating_sub(available_bytes)
            .saturating_sub(keeppeek_bytes);

        let archive_limit = (self.archive_max_bytes > 0).then_some(self.archive_max_bytes);
        let filesystem_limit = has_free_space_limit.then(|| {
            capacity
                .total_bytes
                .saturating_sub(other_used_bytes)
                .saturating_sub(warning_free_bytes)
        });
        let effective_limit_bytes = minimum_limit(archive_limit, filesystem_limit);
        let archive_exceeded = archive_limit.is_some_and(|limit| keeppeek_bytes > limit);
        let free_space_exceeded = has_free_space_limit && available_bytes < warning_free_bytes;
        let cleanup_required = archive_exceeded || free_space_exceeded;
        let cleanup_reason = match (archive_exceeded, free_space_exceeded) {
            (true, true) => Some(StorageCleanupReason::Combined),
            (true, false) => Some(StorageCleanupReason::ArchiveCap),
            (false, true) => Some(StorageCleanupReason::FilesystemHeadroom),
            (false, false) => None,
        };

        let recovery_free_bytes = if has_free_space_limit {
            warning_free_bytes
                .saturating_add(self.cleanup_hysteresis_bytes)
                .min(capacity.total_bytes)
        } else {
            0
        };
        let archive_target =
            archive_limit.map(|limit| limit.saturating_sub(self.cleanup_hysteresis_bytes));
        let filesystem_target = has_free_space_limit.then(|| {
            capacity
                .total_bytes
                .saturating_sub(other_used_bytes)
                .saturating_sub(recovery_free_bytes)
        });
        let cleanup_target_bytes = cleanup_required
            .then(|| minimum_limit(archive_target, filesystem_target).unwrap_or(keeppeek_bytes));

        let pressure = if critical_free_bytes > 0 && available_bytes < critical_free_bytes {
            StoragePressure::Critical
        } else if cleanup_required {
            StoragePressure::Warning
        } else {
            StoragePressure::Normal
        };

        StorageSafetyEvaluation {
            pressure,
            cleanup_required,
            effective_limit_bytes,
            cleanup_target_bytes,
            warning_free_bytes,
            critical_free_bytes,
            recovery_free_bytes,
            cleanup_reason,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StorageSafetyHealthSnapshot {
    pub pressure: StoragePressure,
    pub recording_state: StorageRecordingState,
    pub total_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
    pub keeppeek_bytes: Option<u64>,
    pub effective_limit_bytes: Option<u64>,
    pub cleanup_target_bytes: Option<u64>,
    pub warning_free_bytes: u64,
    pub critical_free_bytes: u64,
    pub recovery_free_bytes: u64,
    pub last_evaluation_at_ms: Option<u64>,
    pub last_evaluation_trigger: Option<StorageCleanupTrigger>,
    pub cleanup_running: bool,
    pub last_cleanup_started_at_ms: Option<u64>,
    pub last_cleanup_ended_at_ms: Option<u64>,
    pub last_cleanup_files_removed: u64,
    pub last_cleanup_bytes_removed: u64,
    pub last_cleanup_reason: Option<StorageCleanupReason>,
    pub last_failure_at_ms: Option<u64>,
    pub last_failure: Option<String>,
    pub last_recovered_at_ms: Option<u64>,
}

impl Default for StorageSafetyHealthSnapshot {
    fn default() -> Self {
        Self {
            pressure: StoragePressure::Normal,
            recording_state: StorageRecordingState::Active,
            total_bytes: None,
            available_bytes: None,
            keeppeek_bytes: None,
            effective_limit_bytes: None,
            cleanup_target_bytes: None,
            warning_free_bytes: 0,
            critical_free_bytes: 0,
            recovery_free_bytes: 0,
            last_evaluation_at_ms: None,
            last_evaluation_trigger: None,
            cleanup_running: false,
            last_cleanup_started_at_ms: None,
            last_cleanup_ended_at_ms: None,
            last_cleanup_files_removed: 0,
            last_cleanup_bytes_removed: 0,
            last_cleanup_reason: None,
            last_failure_at_ms: None,
            last_failure: None,
            last_recovered_at_ms: None,
        }
    }
}

#[derive(Clone, Default)]
pub struct StorageSafetyHealthRegistry {
    inner: Arc<Mutex<StorageSafetyHealthSnapshot>>,
}

impl StorageSafetyHealthRegistry {
    pub fn observe(
        &self,
        trigger: StorageCleanupTrigger,
        capacity: FilesystemCapacity,
        evaluation: StorageSafetyEvaluation,
    ) {
        let mut snapshot = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let was_paused = snapshot.recording_state == StorageRecordingState::Paused;
        snapshot.pressure = evaluation.pressure;
        snapshot.total_bytes = Some(capacity.total_bytes);
        snapshot.available_bytes = Some(capacity.available_bytes);
        snapshot.keeppeek_bytes = Some(capacity.keeppeek_bytes);
        snapshot.effective_limit_bytes = evaluation.effective_limit_bytes;
        snapshot.cleanup_target_bytes = evaluation.cleanup_target_bytes;
        snapshot.warning_free_bytes = evaluation.warning_free_bytes;
        snapshot.critical_free_bytes = evaluation.critical_free_bytes;
        snapshot.recovery_free_bytes = evaluation.recovery_free_bytes;
        snapshot.last_evaluation_at_ms = Some(unix_time_ms());
        snapshot.last_evaluation_trigger = Some(trigger);
        if !snapshot.cleanup_running {
            snapshot.recording_state = match evaluation.pressure {
                StoragePressure::Normal => StorageRecordingState::Active,
                StoragePressure::Warning | StoragePressure::Critical => {
                    StorageRecordingState::Degraded
                }
            };
            if was_paused && snapshot.recording_state == StorageRecordingState::Active {
                snapshot.last_recovered_at_ms = Some(unix_time_ms());
            }
        }
    }

    pub fn cleanup_started(&self, reason: StorageCleanupReason) {
        let mut snapshot = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        snapshot.cleanup_running = true;
        snapshot.recording_state = StorageRecordingState::Degraded;
        snapshot.last_cleanup_started_at_ms = Some(unix_time_ms());
        snapshot.last_cleanup_ended_at_ms = None;
        snapshot.last_cleanup_files_removed = 0;
        snapshot.last_cleanup_bytes_removed = 0;
        snapshot.last_cleanup_reason = Some(reason);
    }

    pub fn cleanup_finished(&self, files_removed: u64, bytes_removed: u64) {
        let mut snapshot = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        snapshot.cleanup_running = false;
        snapshot.last_cleanup_ended_at_ms = Some(unix_time_ms());
        snapshot.last_cleanup_files_removed = files_removed;
        snapshot.last_cleanup_bytes_removed = bytes_removed;
        snapshot.recording_state = match snapshot.pressure {
            StoragePressure::Normal => StorageRecordingState::Active,
            StoragePressure::Warning | StoragePressure::Critical => StorageRecordingState::Degraded,
        };
    }

    pub fn cleanup_progress(&self, bytes_removed: u64) {
        if bytes_removed == 0 {
            return;
        }
        let mut snapshot = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        snapshot.last_cleanup_files_removed = snapshot.last_cleanup_files_removed.saturating_add(1);
        snapshot.last_cleanup_bytes_removed = snapshot
            .last_cleanup_bytes_removed
            .saturating_add(bytes_removed);
    }

    pub fn cleanup_failed(&self, error: &str) {
        let mut snapshot = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = unix_time_ms();
        snapshot.cleanup_running = false;
        snapshot.pressure = StoragePressure::Critical;
        snapshot.recording_state = StorageRecordingState::Paused;
        snapshot.last_cleanup_ended_at_ms = Some(now);
        snapshot.last_failure_at_ms = Some(now);
        snapshot.last_failure = Some(error.chars().take(MAX_ERROR_CHARS).collect());
    }

    pub fn recording_paused(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recording_state
            == StorageRecordingState::Paused
    }

    pub fn snapshot(&self) -> StorageSafetyHealthSnapshot {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

pub fn filesystem_capacity(
    path: &Path,
    keeppeek_bytes: u64,
) -> std::io::Result<FilesystemCapacity> {
    let path = absolute_path(path)?;
    let query_path = nearest_existing_path(&path)?;
    let stats = fs4::statvfs(query_path)?;
    Ok(FilesystemCapacity {
        total_bytes: stats.total_space(),
        available_bytes: stats.available_space(),
        keeppeek_bytes,
    })
}

fn nearest_existing_path(path: &Path) -> std::io::Result<&Path> {
    let mut candidate = path;
    loop {
        match std::fs::metadata(candidate) {
            Ok(_) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                candidate = candidate.parent().ok_or(error)?;
            }
            Err(error) => return Err(error),
        }
    }
}

fn absolute_path(path: &Path) -> std::io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn minimum_limit(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(limit), None) | (None, Some(limit)) => Some(limit),
        (None, None) => None,
    }
}

fn percent_of(value: u64, percent: u8) -> u64 {
    let percent = u64::from(percent.min(100));
    value / 100 * percent + value % 100 * percent / 100
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIBIBYTE: u64 = 1_073_741_824;

    fn policy() -> StorageSafetyPolicy {
        StorageSafetyPolicy {
            archive_max_bytes: 0,
            minimum_free_bytes: 0,
            maximum_used_percent: None,
            warning_free_bytes: 0,
            critical_free_bytes: 0,
            cleanup_hysteresis_bytes: 0,
        }
    }

    fn capacity(available_gib: u64, keeppeek_gib: u64) -> FilesystemCapacity {
        FilesystemCapacity {
            total_bytes: 100 * GIBIBYTE,
            available_bytes: available_gib * GIBIBYTE,
            keeppeek_bytes: keeppeek_gib * GIBIBYTE,
        }
    }

    #[test]
    fn unlimited_policy_never_requests_cleanup() {
        let evaluation = policy().evaluate(capacity(1, 90));

        assert_eq!(evaluation.pressure, StoragePressure::Normal);
        assert!(!evaluation.cleanup_required);
        assert_eq!(evaluation.effective_limit_bytes, None);
        assert_eq!(evaluation.cleanup_target_bytes, None);
    }

    #[test]
    fn archive_cap_recovers_past_hysteresis() {
        let evaluation = StorageSafetyPolicy {
            archive_max_bytes: 50 * GIBIBYTE,
            cleanup_hysteresis_bytes: 5 * GIBIBYTE,
            ..policy()
        }
        .evaluate(capacity(30, 60));

        assert_eq!(evaluation.pressure, StoragePressure::Warning);
        assert_eq!(evaluation.effective_limit_bytes, Some(50 * GIBIBYTE));
        assert_eq!(evaluation.cleanup_target_bytes, Some(45 * GIBIBYTE));
    }

    #[test]
    fn reserve_accounts_for_non_keeppeek_disk_usage() {
        let evaluation = StorageSafetyPolicy {
            minimum_free_bytes: 15 * GIBIBYTE,
            warning_free_bytes: 20 * GIBIBYTE,
            critical_free_bytes: 15 * GIBIBYTE,
            cleanup_hysteresis_bytes: 5 * GIBIBYTE,
            ..policy()
        }
        .evaluate(capacity(10, 60));

        assert_eq!(evaluation.pressure, StoragePressure::Critical);
        assert_eq!(evaluation.effective_limit_bytes, Some(50 * GIBIBYTE));
        assert_eq!(evaluation.cleanup_target_bytes, Some(45 * GIBIBYTE));
        assert_eq!(evaluation.recovery_free_bytes, 25 * GIBIBYTE);
    }

    #[test]
    fn percentage_limit_uses_filesystem_capacity() {
        let evaluation = StorageSafetyPolicy {
            maximum_used_percent: Some(80),
            cleanup_hysteresis_bytes: 5 * GIBIBYTE,
            ..policy()
        }
        .evaluate(capacity(15, 50));

        assert_eq!(evaluation.pressure, StoragePressure::Warning);
        assert_eq!(evaluation.warning_free_bytes, 20 * GIBIBYTE);
        assert_eq!(evaluation.effective_limit_bytes, Some(45 * GIBIBYTE));
        assert_eq!(evaluation.cleanup_target_bytes, Some(40 * GIBIBYTE));
    }

    #[test]
    fn combined_policy_uses_the_tightest_limit() {
        let evaluation = StorageSafetyPolicy {
            archive_max_bytes: 70 * GIBIBYTE,
            minimum_free_bytes: 10 * GIBIBYTE,
            maximum_used_percent: Some(75),
            warning_free_bytes: 20 * GIBIBYTE,
            critical_free_bytes: 10 * GIBIBYTE,
            cleanup_hysteresis_bytes: 5 * GIBIBYTE,
        }
        .evaluate(capacity(20, 60));

        assert_eq!(evaluation.effective_limit_bytes, Some(55 * GIBIBYTE));
        assert_eq!(evaluation.cleanup_target_bytes, Some(50 * GIBIBYTE));
    }

    #[test]
    fn cleanup_stays_disarmed_after_reaching_recovery_target() {
        let policy = StorageSafetyPolicy {
            warning_free_bytes: 20 * GIBIBYTE,
            critical_free_bytes: 10 * GIBIBYTE,
            cleanup_hysteresis_bytes: 5 * GIBIBYTE,
            ..policy()
        };

        let pressured = policy.evaluate(capacity(19, 60));
        let recovered = policy.evaluate(capacity(25, 54));

        assert!(pressured.cleanup_required);
        assert_eq!(pressured.cleanup_target_bytes, Some(54 * GIBIBYTE));
        assert!(!recovered.cleanup_required);
        assert_eq!(recovered.pressure, StoragePressure::Normal);
    }

    #[test]
    fn rapid_growth_crosses_warning_before_critical_pressure() {
        let policy = StorageSafetyPolicy {
            warning_free_bytes: 20 * GIBIBYTE,
            critical_free_bytes: 10 * GIBIBYTE,
            cleanup_hysteresis_bytes: 5 * GIBIBYTE,
            ..policy()
        };

        let normal = policy.evaluate(capacity(21, 60));
        let warning = policy.evaluate(capacity(19, 62));
        let critical = policy.evaluate(capacity(9, 72));

        assert_eq!(normal.pressure, StoragePressure::Normal);
        assert!(!normal.cleanup_required);
        assert_eq!(warning.pressure, StoragePressure::Warning);
        assert!(warning.cleanup_required);
        assert_eq!(critical.pressure, StoragePressure::Critical);
        assert!(critical.cleanup_required);
    }

    #[test]
    fn filesystem_capacity_queries_the_nearest_existing_parent() {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-storage-capacity-{}",
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let capacity = filesystem_capacity(&root.join("not-created/archive"), 42).unwrap();

        assert!(capacity.total_bytes > 0);
        assert!(capacity.available_bytes <= capacity.total_bytes);
        assert_eq!(capacity.keeppeek_bytes, 42);
        std::fs::remove_dir_all(root).unwrap();
    }
}
