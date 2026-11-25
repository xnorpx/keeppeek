//! Durable internal telemetry identity, schedule, and pending-deletion state.
//!
//! Stored in an application-owned private file separate from `config.toml` and `StateStore`
//! so configuration backups never clone an installation's proof token across machines and
//! general StateStore clients cannot inspect or mutate telemetry identity material.

use base64::Engine as _;
use rand::Rng as _;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub const STATE_SCHEMA_VERSION: u32 = 1;
pub const MAX_PENDING_DELETIONS: usize = 16;
pub const PLACEHOLDER_PREVIEW_UUID: &str = "00000000-0000-4000-8000-000000000000";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    #[default]
    None,
    ReceiverUnconfigured,
    PayloadTooLarge,
    ValidationFailed,
    NetworkUnavailable,
    DnsFailed,
    TlsFailed,
    Timeout,
    RateLimited,
    ReceiverRejected,
    ReceiverUnavailable,
    InvalidResponse,
    DeletionFailed,
    StorageError,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleState {
    Disabled,
    Scheduled,
    Sending,
    Succeeded,
    Failed,
    DeletionPending,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationIdentity {
    pub installation_id: String,
    pub installation_token: String,
    pub report_sequence: u64,
    pub created_at_ms: i64,
}

impl std::fmt::Debug for InstallationIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InstallationIdentity")
            .field("installation_id", &"[redacted]")
            .field("installation_token", &"[redacted]")
            .field("report_sequence", &self.report_sequence)
            .field("created_at_ms", &self.created_at_ms)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingDeletion {
    pub installation_id: String,
    pub installation_token: String,
    pub requested_at_ms: i64,
}

impl std::fmt::Debug for PendingDeletion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingDeletion")
            .field("installation_id", &"[redacted]")
            .field("installation_token", &"[redacted]")
            .field("requested_at_ms", &self.requested_at_ms)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetryRuntimeState {
    #[serde(default = "default_state_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<InstallationIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_scheduled_at_ms: Option<i64>,
    #[serde(default)]
    pub last_failure: FailureKind,
    #[serde(default)]
    pub pending_deletions: Vec<PendingDeletion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_deletion_attempt_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_deletion_success_at_ms: Option<i64>,
    #[serde(default)]
    pub last_deletion_failure: FailureKind,
}

const fn default_state_version() -> u32 {
    STATE_SCHEMA_VERSION
}

impl Default for TelemetryRuntimeState {
    fn default() -> Self {
        Self {
            version: STATE_SCHEMA_VERSION,
            identity: None,
            last_attempt_at_ms: None,
            last_success_at_ms: None,
            next_scheduled_at_ms: None,
            last_failure: FailureKind::None,
            pending_deletions: Vec::new(),
            last_deletion_attempt_at_ms: None,
            last_deletion_success_at_ms: None,
            last_deletion_failure: FailureKind::None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateError {
    PendingDeletionQueueFull,
    PersistenceFailed,
    CorruptState,
}

impl std::fmt::Display for StateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::PendingDeletionQueueFull => {
                "pending deletion queue reached its 16-entry limit; retry deletion before resetting again"
            }
            Self::PersistenceFailed => "failed to persist usage reporting state",
            Self::CorruptState => "persisted usage reporting state is invalid",
        })
    }
}

impl std::error::Error for StateError {}

impl TelemetryRuntimeState {
    /// Loads existing telemetry state without creating any file or UUID when absent.
    pub fn load_if_present(path: &Path) -> Result<Self, StateError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read(path).map_err(|_| StateError::PersistenceFailed)?;
        let state: Self = serde_json::from_slice(&raw).map_err(|_| StateError::CorruptState)?;
        if state.version != STATE_SCHEMA_VERSION
            || state.pending_deletions.len() > MAX_PENDING_DELETIONS
        {
            return Err(StateError::CorruptState);
        }
        Ok(state)
    }

    pub fn save_atomically(&self, path: &Path) -> Result<(), StateError> {
        if self.pending_deletions.len() > MAX_PENDING_DELETIONS {
            return Err(StateError::PendingDeletionQueueFull);
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(|_| StateError::PersistenceFailed)?;
        crate::config::write_private_file_atomically(path, &bytes)
            .map_err(|_| StateError::PersistenceFailed)
    }

    pub fn ensure_identity(&mut self, now_ms: i64) -> &InstallationIdentity {
        if self.identity.is_none() {
            self.identity = Some(generate_identity(now_ms));
        }
        self.identity
            .as_ref()
            .expect("identity was just initialized")
    }

    /// Rotates the active telemetry UUID and queues deletion of the prior receiver row.
    pub fn reset_identity(&mut self, now_ms: i64, keep_enabled: bool) -> Result<(), StateError> {
        if let Some(previous) = self.identity.as_ref() {
            if self.pending_deletions.len() >= MAX_PENDING_DELETIONS {
                return Err(StateError::PendingDeletionQueueFull);
            }
            self.pending_deletions.push(PendingDeletion {
                installation_id: previous.installation_id.clone(),
                installation_token: previous.installation_token.clone(),
                requested_at_ms: now_ms,
            });
        }
        self.identity = keep_enabled.then(|| generate_identity(now_ms));
        if !keep_enabled {
            self.next_scheduled_at_ms = None;
        }
        Ok(())
    }

    /// Disables telemetry immediately and moves any active identity into the pending-deletion queue.
    pub fn disable_and_queue_deletion(&mut self, now_ms: i64) -> Result<(), StateError> {
        self.next_scheduled_at_ms = None;
        if let Some(previous) = self.identity.as_ref() {
            if self.pending_deletions.len() >= MAX_PENDING_DELETIONS {
                return Err(StateError::PendingDeletionQueueFull);
            }
            self.pending_deletions.push(PendingDeletion {
                installation_id: previous.installation_id.clone(),
                installation_token: previous.installation_token.clone(),
                requested_at_ms: now_ms,
            });
            self.identity = None;
        }
        Ok(())
    }

    pub const fn lifecycle_state(&self, enabled: bool, sending: bool) -> LifecycleState {
        if sending {
            return LifecycleState::Sending;
        }
        if !enabled {
            return if !self.pending_deletions.is_empty() {
                LifecycleState::DeletionPending
            } else {
                LifecycleState::Disabled
            };
        }
        if !self.pending_deletions.is_empty() {
            return LifecycleState::DeletionPending;
        }
        if !matches!(self.last_failure, FailureKind::None) {
            return LifecycleState::Failed;
        }
        if self.last_success_at_ms.is_some() {
            return LifecycleState::Succeeded;
        }
        LifecycleState::Scheduled
    }
}

pub fn default_state_path(config_path: &Path) -> PathBuf {
    let parent = config_path.parent().unwrap_or_else(|| Path::new("."));
    parent.join(".keeppeek-usage-reporting-state.json")
}

fn generate_identity(now_ms: i64) -> InstallationIdentity {
    let mut token_bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut token_bytes);
    let installation_token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token_bytes);
    InstallationIdentity {
        installation_id: Uuid::new_v4().to_string(),
        installation_token,
        report_sequence: 0,
        created_at_ms: now_ms,
    }
}
