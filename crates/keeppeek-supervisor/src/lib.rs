//! Offline multi-executable A/B supervisor for KeepPeek and KeepPeek Vision.
//!
//! This crate manages versioned standalone child executables under an immutable
//! slot layout, verifies offline Ed25519 release manifests and SHA-256 digests,
//! supervises child lifecycles with OS process-group or Job Object isolation,
//! and rolls back failing upgrades automatically.
//!
//! All three slim host shells (`keeppeek-service`, `keeppeek-supervisor`, and
//! opt-in managed local server mode in `keeppeek-desktop`) embed this crate.

pub mod manifest;
pub mod process;
pub mod slots;
pub mod supervisor;
pub mod version;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Exit code returned by a supervised child to request a clean supervisor restart.
///
/// The supervisor waits for the child process to exit so file locks and sockets
/// release cleanly before launching a pending upgrade or restarting the slot.
pub const EXIT_CODE_RESTART_REQUESTED: u8 = 75;

/// Environment variable set to `"1"` when a child process runs under a supervisor.
pub const ENV_SUPERVISED: &str = "KEEPPEEK_SUPERVISED";

/// Environment variable naming the readiness marker file written by a child process.
pub const ENV_READY_FILE: &str = "KEEPPEEK_SUPERVISOR_READY_FILE";

/// Environment variable naming the stop sentinel file created by the supervisor.
pub const ENV_STOP_FILE: &str = "KEEPPEEK_SUPERVISOR_STOP_FILE";

/// Environment variable passing the active slot version string to a child process.
pub const ENV_ACTIVE_VERSION: &str = "KEEPPEEK_SUPERVISOR_VERSION";

/// Environment variable passing the supervised component name to a child process.
pub const ENV_COMPONENT: &str = "KEEPPEEK_SUPERVISOR_COMPONENT";

/// JSON payload written atomically by a child process when startup reaches readiness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadyMarker {
    /// Operating system process identifier of the ready child.
    pub pid: u32,
    /// Application version reported by the child process.
    pub version: String,
    /// Confirmation flag that startup completed cleanly.
    pub ready: bool,
}

/// Returns whether the current process was launched under a KeepPeek supervisor.
#[must_use]
pub fn is_supervised_process() -> bool {
    std::env::var(ENV_SUPERVISED)
        .map(|value| value.trim() == "1")
        .unwrap_or(false)
}

/// Returns the stop sentinel path configured for the current child process, if any.
#[must_use]
pub fn stop_file_from_env() -> Option<PathBuf> {
    std::env::var_os(ENV_STOP_FILE)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Returns true when the supervisor has created the stop sentinel file.
#[must_use]
pub fn stop_requested(stop_file: Option<&Path>) -> bool {
    stop_file.is_some_and(Path::exists)
}

/// Writes the child readiness marker atomically when `KEEPPEEK_SUPERVISOR_READY_FILE` is set.
///
/// Does nothing and returns `Ok(())` when the environment variable is unset.
///
/// # Errors
///
/// Returns an I/O error when creating parent directories, writing the temporary
/// file, syncing, or atomically renaming into place fails.
pub fn signal_ready_from_env(version: &str) -> std::io::Result<()> {
    let Some(path) = std::env::var_os(ENV_READY_FILE).filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    write_ready_marker(Path::new(&path), std::process::id(), version)
}

/// Writes a [`ReadyMarker`] atomically to `path`.
///
/// # Errors
///
/// Returns an I/O error if serialization, file creation, syncing, or rename fails.
pub fn write_ready_marker(path: &Path, pid: u32, version: &str) -> std::io::Result<()> {
    let marker = ReadyMarker {
        pid,
        version: version.to_owned(),
        ready: true,
    };
    let payload = serde_json::to_vec(&marker).map_err(std::io::Error::other)?;
    if let Some(parent) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let temp_path = path.with_extension(format!("ready-{pid}.tmp"));
    let mut file = fs::File::create(&temp_path)?;
    file.write_all(&payload)?;
    file.sync_all()?;
    drop(file);
    if let Err(error) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    Ok(())
}
