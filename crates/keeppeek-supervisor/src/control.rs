//! Live runtime IPC primitives shared across CLI, Windows SCM service, and Desktop hosts.
//!
//! All live supervisor coordination lives under `<supervisor_root>/runtime/`:
//! - `supervisor.lock`: exclusive OS file lock held for [`crate::supervisor::SupervisorHandle`]'s lifetime.
//! - `supervisor-status.json`: bounded atomic snapshot of live worker phases, PIDs, and control IDs.
//! - `control-<component>.json`: bounded atomic command file read and unlinked before child drain.

use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::acl::create_private_dir_all;
use crate::manifest::ComponentKind;
use crate::slots::write_atomically;
use crate::supervisor::{ComponentPhase, ComponentSnapshot};

/// Schema version for `runtime/supervisor-status.json` and `runtime/control-*.json`.
pub const CONTROL_SCHEMA_VERSION_V1: u32 = 1;

/// Maximum serialized byte size allowed for `runtime/supervisor-status.json`.
pub const MAX_STATUS_FILE_BYTES: u64 = 64 * 1024;

/// Maximum serialized byte size allowed for `runtime/control-<component>.json`.
pub const MAX_CONTROL_FILE_BYTES: u64 = 4 * 1024;

/// Maximum length of a `request_id` string in bytes.
pub const MAX_REQUEST_ID_BYTES: usize = 64;

const SUPERVISOR_LOCK_FILE: &str = "supervisor.lock";
const LIVE_STATUS_FILE: &str = "supervisor-status.json";
const WAIT_POLL_INTERVAL_MS: u64 = 40;

static CONTROL_REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);

const _: () = {
    assert!(MAX_CONTROL_FILE_BYTES > 0);
    assert!(MAX_STATUS_FILE_BYTES >= MAX_CONTROL_FILE_BYTES);
    assert!(MAX_REQUEST_ID_BYTES >= 16);
};

/// External live action delivered to a running supervisor via `<root>/runtime/control-<component>.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlAction {
    /// Drain and restart only when a `pending` slot exists for the target component.
    ApplyPending,
    /// Unconditionally drain and restart the target component.
    Restart,
}

/// Bounded command payload written by CLI tools and consumed once by the live supervisor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlRequest {
    /// Control schema version (must be [`CONTROL_SCHEMA_VERSION_V1`]).
    pub schema_version: u32,
    /// Unique identifier echoed into `last_control_id` when the request completes.
    pub request_id: String,
    /// Requested supervisor action.
    pub action: ControlAction,
    /// Unix timestamp in milliseconds when the request was issued.
    pub requested_at_unix_ms: u64,
}

/// Live supervisor status snapshot written atomically to `<root>/runtime/supervisor-status.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveSupervisorStatus {
    /// Status schema version (must be [`CONTROL_SCHEMA_VERSION_V1`]).
    pub schema_version: u32,
    /// Operating system process identifier of the owning supervisor host.
    pub supervisor_pid: u32,
    /// True when the running supervisor host opted into `--allow-local`.
    pub allow_local: bool,
    /// Unix timestamp in milliseconds when this snapshot was persisted.
    pub updated_at_unix_ms: u64,
    /// Core `keeppeek` component runtime status.
    pub core: ComponentSnapshot,
    /// Optional `keeppeek-vision` component runtime status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<ComponentSnapshot>,
}

impl LiveSupervisorStatus {
    /// Returns the component snapshot for `component`, if enabled.
    #[must_use]
    pub const fn component(&self, component: ComponentKind) -> Option<&ComponentSnapshot> {
        match component {
            ComponentKind::Core => Some(&self.core),
            ComponentKind::Vision => self.vision.as_ref(),
        }
    }
}

/// Errors produced when coordinating live supervisor lock, status, or control IPC.
#[derive(Debug)]
pub enum ControlError {
    /// Another supervisor process already holds `<root>/runtime/supervisor.lock`.
    AlreadyRunning { lock_path: PathBuf },
    /// No live supervisor currently holds `<root>/runtime/supervisor.lock`.
    SupervisorNotRunning { root: PathBuf },
    /// Requested component is not enabled in the live supervisor.
    ComponentNotEnabled(ComponentKind),
    /// A pending upgrade triggered by this control request failed probation and rolled back.
    RolledBack { snapshot: Box<ComponentSnapshot> },
    /// Target component entered [`ComponentPhase::Failed`] while processing the control request.
    ApplyFailed {
        component: ComponentKind,
        detail: String,
    },
    /// Timed out waiting for the live supervisor to finish processing `request_id`.
    WaitTimedOut {
        component: ComponentKind,
        request_id: String,
        timeout_ms: u64,
    },
    /// Invalid or unsupported status/control payload.
    InvalidPayload(String),
    /// Filesystem I/O failed.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for ControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRunning { lock_path } => write!(
                formatter,
                "another KeepPeek supervisor is already running on {}",
                lock_path.display()
            ),
            Self::SupervisorNotRunning { root } => write!(
                formatter,
                "no live KeepPeek supervisor is running on {}",
                root.display()
            ),
            Self::ComponentNotEnabled(component) => write!(
                formatter,
                "component `{component}` is not enabled in the running supervisor"
            ),
            Self::RolledBack { snapshot } => write!(
                formatter,
                "component `{}` rolled back after failed probation (running slot: {:?}, detail: {})",
                snapshot.component,
                snapshot.running_slot,
                snapshot
                    .last_error
                    .as_deref()
                    .unwrap_or("pending release failed health check")
            ),
            Self::ApplyFailed { component, detail } => {
                write!(
                    formatter,
                    "component `{component}` failed during apply: {detail}"
                )
            }
            Self::WaitTimedOut {
                component,
                request_id,
                timeout_ms,
            } => write!(
                formatter,
                "timed out after {timeout_ms} ms waiting for `{component}` control request `{request_id}`"
            ),
            Self::InvalidPayload(detail) => {
                write!(formatter, "invalid supervisor IPC payload: {detail}")
            }
            Self::Io { path, source } => write!(
                formatter,
                "supervisor runtime IPC I/O failed at {}: {source}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ControlError {}

/// Exclusive OS lock held for the lifetime of a live [`crate::supervisor::SupervisorHandle`].
pub struct SupervisorProcessLock {
    _file: File,
}

impl SupervisorProcessLock {
    /// Acquires `<runtime_dir>/supervisor.lock` without blocking, failing if already held.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::AlreadyRunning`] if another supervisor owns `runtime_dir`.
    pub fn try_acquire(runtime_dir: &Path) -> Result<Self, ControlError> {
        create_private_dir_all(runtime_dir).map_err(|source| ControlError::Io {
            path: runtime_dir.to_path_buf(),
            source,
        })?;
        let lock_path = runtime_dir.join(SUPERVISOR_LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| ControlError::Io {
                path: lock_path.clone(),
                source,
            })?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(TryLockError::WouldBlock) => Err(ControlError::AlreadyRunning { lock_path }),
            Err(TryLockError::Error(source)) => Err(ControlError::Io {
                path: lock_path,
                source,
            }),
        }
    }
}

/// Returns true when a live supervisor currently holds `<root>/runtime/supervisor.lock`.
#[must_use]
pub fn is_supervisor_running(root: &Path) -> bool {
    let lock_path = root.join("runtime").join(SUPERVISOR_LOCK_FILE);
    let Ok(file) = OpenOptions::new().read(true).write(true).open(&lock_path) else {
        return false;
    };
    match file.try_lock() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(TryLockError::WouldBlock) => true,
        Err(TryLockError::Error(_)) => false,
    }
}

/// Returns the control file path `<root>/runtime/control-<component>.json`.
#[must_use]
pub fn control_file_path(root: &Path, component: ComponentKind) -> PathBuf {
    root.join("runtime")
        .join(format!("control-{}.json", component.as_str()))
}

/// Returns the live status file path `<root>/runtime/supervisor-status.json`.
#[must_use]
pub fn live_status_path(root: &Path) -> PathBuf {
    root.join("runtime").join(LIVE_STATUS_FILE)
}

/// Removes any leftover `control-<component>.json` file on supervisor startup.
pub fn cleanup_stale_control_file(root: &Path, component: ComponentKind) {
    let _ = fs::remove_file(control_file_path(root, component));
}

/// Persists `status` atomically to `<root>/runtime/supervisor-status.json`.
///
/// # Errors
///
/// Returns [`ControlError`] if serialization or atomic file replacement fails.
pub fn write_live_status(root: &Path, status: &LiveSupervisorStatus) -> Result<(), ControlError> {
    let bytes = serde_json::to_vec_pretty(status)
        .map_err(|error| ControlError::InvalidPayload(error.to_string()))?;
    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if len > MAX_STATUS_FILE_BYTES {
        return Err(ControlError::InvalidPayload(format!(
            "live status size {len} exceeds {MAX_STATUS_FILE_BYTES} bytes"
        )));
    }
    let path = live_status_path(root);
    write_atomically(&path, &bytes).map_err(|error| ControlError::Io {
        path,
        source: std::io::Error::other(error),
    })
}

/// Reads `<root>/runtime/supervisor-status.json` if present and valid.
///
/// # Errors
///
/// Returns [`ControlError`] when an existing file is oversized, unreadable, or malformed.
pub fn read_live_status(root: &Path) -> Result<Option<LiveSupervisorStatus>, ControlError> {
    let path = live_status_path(root);
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(ControlError::Io { path, source }),
    };
    let mut bytes = Vec::new();
    file.take(MAX_STATUS_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| ControlError::Io {
            path: path.clone(),
            source,
        })?;
    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if len > MAX_STATUS_FILE_BYTES {
        return Err(ControlError::InvalidPayload(format!(
            "live status file is {len} bytes, exceeding {MAX_STATUS_FILE_BYTES} bytes"
        )));
    }
    let status: LiveSupervisorStatus = serde_json::from_slice(&bytes)
        .map_err(|error| ControlError::InvalidPayload(error.to_string()))?;
    if status.schema_version != CONTROL_SCHEMA_VERSION_V1 {
        return Err(ControlError::InvalidPayload(format!(
            "unsupported live status schema {}",
            status.schema_version
        )));
    }
    Ok(Some(status))
}

/// Atomically writes a new control request to `<root>/runtime/control-<component>.json`.
///
/// # Errors
///
/// Returns [`ControlError`] if serialization or atomic persistence fails.
pub fn write_control_request(
    root: &Path,
    component: ComponentKind,
    action: ControlAction,
) -> Result<ControlRequest, ControlError> {
    let now_ms = current_unix_time_ms();
    let seq = CONTROL_REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let request_id = format!("req-{}-{now_ms}-{seq}", std::process::id());
    debug_assert!(is_valid_request_id(&request_id));
    let request = ControlRequest {
        schema_version: CONTROL_SCHEMA_VERSION_V1,
        request_id,
        action,
        requested_at_unix_ms: now_ms,
    };
    let bytes = serde_json::to_vec(&request)
        .map_err(|error| ControlError::InvalidPayload(error.to_string()))?;
    create_private_dir_all(root).map_err(|source| ControlError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    let path = control_file_path(root, component);
    write_atomically(&path, &bytes).map_err(|error| ControlError::Io {
        path,
        source: std::io::Error::other(error),
    })?;
    Ok(request)
}

/// Atomically claims and removes `<root>/runtime/control-<component>.json` if present.
///
/// Renaming to a private claim path before reading ensures a concurrent writer
/// cannot be silently deleted between read and unlink, and removing before draining
/// prevents replay loops across crashes.
#[must_use]
pub fn take_control_request(root: &Path, component: ComponentKind) -> Option<ControlRequest> {
    let path = control_file_path(root, component);
    let seq = CONTROL_REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let claimed = root.join("runtime").join(format!(
        ".control-{}.claim-{}-{seq}.tmp",
        component.as_str(),
        std::process::id()
    ));
    if fs::rename(&path, &claimed).is_err() {
        return None;
    }
    let mut bytes = Vec::new();
    let read_ok = File::open(&claimed).ok().is_some_and(|file| {
        file.take(MAX_CONTROL_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .is_ok()
    });
    let _ = fs::remove_file(&claimed);
    if !read_ok || u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_CONTROL_FILE_BYTES {
        return None;
    }
    let request: ControlRequest = serde_json::from_slice(&bytes).ok()?;
    if request.schema_version != CONTROL_SCHEMA_VERSION_V1
        || !is_valid_request_id(&request.request_id)
    {
        return None;
    }
    Some(request)
}

/// Waits until the live supervisor finishes processing `request_id` for `component`.
///
/// Fails fast when no supervisor holds `runtime/supervisor.lock`, when the component
/// rolls back to its fallback slot, or when the component enters [`ComponentPhase::Failed`].
///
/// # Errors
///
/// Returns [`ControlError`] on missing supervisor, disabled component, rollback,
/// terminal failure, or timeout.
pub fn wait_for_control_completion(
    root: &Path,
    component: ComponentKind,
    request_id: &str,
    timeout_ms: u64,
) -> Result<ComponentSnapshot, ControlError> {
    if !is_supervisor_running(root) {
        return Err(ControlError::SupervisorNotRunning {
            root: root.to_path_buf(),
        });
    }
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(1));
    while Instant::now() < deadline {
        if !is_supervisor_running(root) {
            return Err(ControlError::SupervisorNotRunning {
                root: root.to_path_buf(),
            });
        }
        if let Ok(Some(live)) = read_live_status(root) {
            let Some(comp) = live.component(component) else {
                return Err(ControlError::ComponentNotEnabled(component));
            };
            if comp.last_control_id.as_deref() == Some(request_id) {
                if comp.phase == ComponentPhase::Failed {
                    return Err(ControlError::ApplyFailed {
                        component,
                        detail: comp
                            .last_error
                            .clone()
                            .unwrap_or_else(|| "component failed during apply".to_owned()),
                    });
                }
                if comp.last_control_rolled_back {
                    return Err(ControlError::RolledBack {
                        snapshot: Box::new(comp.clone()),
                    });
                }
                if comp.phase == ComponentPhase::Running {
                    return Ok(comp.clone());
                }
            }
        }
        thread::sleep(Duration::from_millis(WAIT_POLL_INTERVAL_MS));
    }
    Err(ControlError::WaitTimedOut {
        component,
        request_id: request_id.to_owned(),
        timeout_ms,
    })
}

/// Returns current whole-millisecond Unix timestamp (saturating to `u64::MAX`).
#[must_use]
pub fn current_unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

fn is_valid_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_REQUEST_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}
