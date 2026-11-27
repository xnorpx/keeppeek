//! Cross-platform child process lifecycle management and OS cleanup guards.
//!
//! On Windows, every spawned child is assigned to a Win32 Job Object with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` so children cannot outlive the parent
//! supervisor. On Unix, each child runs in its own process group so graceful
//! `SIGTERM` and forced `SIGKILL` signals cover the entire child subtree.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
#[cfg(unix)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use tracing::warn;

use crate::manifest::ComponentKind;
use crate::version::ReleaseVersion;
use crate::{
    ENV_ACTIVE_VERSION, ENV_COMPONENT, ENV_READY_FILE, ENV_STOP_FILE, ENV_SUPERVISED,
    EXIT_CODE_RESTART_REQUESTED, ReadyMarker,
};

const MAX_READY_FILE_BYTES: u64 = 4096;
const STOP_POLL_INTERVAL_MS: u64 = 25;
static RUN_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Parameters describing how to spawn one supervised child executable.
#[derive(Debug, Clone)]
pub struct ChildSpawnSpec {
    /// Supervised component represented by the child process.
    pub component: ComponentKind,
    /// Executable path to spawn.
    pub binary_path: PathBuf,
    /// Managed slot version if launched from an immutable A/B slot.
    pub version: Option<ReleaseVersion>,
    /// Command-line arguments passed to the child executable.
    pub args: Vec<OsString>,
    /// Additional environment variables set on the child process.
    pub extra_env: Vec<(String, String)>,
    /// Directory storing ephemeral ready/stop sentinel files.
    pub runtime_dir: PathBuf,
    /// Suppresses console window creation on Windows (used by SCM and Tauri hosts).
    pub hide_console_window: bool,
}

/// Normalized exit classification returned after a child process terminates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildExitStatus {
    /// Child exited cleanly with exit code 0.
    CleanExit,
    /// Child exited with [`EXIT_CODE_RESTART_REQUESTED`] to request a supervisor restart.
    RestartRequested,
    /// Child exited with another non-zero exit code.
    FailedCode(i32),
    /// Child terminated due to a signal or forced kill.
    Terminated,
}

impl ChildExitStatus {
    #[must_use]
    pub(crate) fn from_exit_status(status: ExitStatus) -> Self {
        match status.code() {
            Some(0) => Self::CleanExit,
            Some(code) if code == i32::from(EXIT_CODE_RESTART_REQUESTED) => Self::RestartRequested,
            Some(code) => Self::FailedCode(code),
            None => Self::Terminated,
        }
    }
}

/// Errors produced when spawning or polling a managed child process.
#[derive(Debug)]
pub enum ProcessError {
    /// Spawning the executable or preparing sentinel files failed.
    Spawn {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Assigning the Windows Job Object to the spawned child failed.
    #[cfg(windows)]
    JobObject { detail: String },
    /// Polling child process status failed.
    Wait { source: std::io::Error },
}

impl fmt::Display for ProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn { path, source } => {
                write!(formatter, "unable to spawn {}: {source}", path.display())
            }
            #[cfg(windows)]
            Self::JobObject { detail } => {
                write!(formatter, "unable to attach Windows Job Object: {detail}")
            }
            Self::Wait { source } => {
                write!(
                    formatter,
                    "unable to poll supervised child process: {source}"
                )
            }
        }
    }
}

impl std::error::Error for ProcessError {}

/// Removes stale ready and stop sentinel files left behind by a prior supervisor crash.
pub fn cleanup_stale_sentinels(runtime_dir: &Path, component: ComponentKind) {
    let Ok(entries) = fs::read_dir(runtime_dir) else {
        return;
    };
    let prefix = format!("{}-", component.as_str());
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if name.starts_with(&prefix)
            && (name.ends_with(".ready.json") || name.ends_with(".stop") || name.ends_with(".tmp"))
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Live handle to one supervised child executable and its OS cleanup guard.
pub struct ManagedChild {
    child: Child,
    pid: u32,
    ready_file: PathBuf,
    stop_file: PathBuf,
    last_status: Option<ChildExitStatus>,
    #[cfg(unix)]
    group_killed: AtomicBool,
    #[cfg(windows)]
    _job_guard: WindowsJobGuard,
}

impl ManagedChild {
    /// Spawns a child process under OS lifecycle guards and returns a [`ManagedChild`].
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError`] when creating runtime directories, spawning the
    /// executable, or attaching OS cleanup guards fails.
    pub fn spawn(spec: &ChildSpawnSpec) -> Result<Self, ProcessError> {
        fs::create_dir_all(&spec.runtime_dir).map_err(|source| ProcessError::Spawn {
            path: spec.runtime_dir.clone(),
            source,
        })?;
        let run_id = RUN_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        let prefix = format!(
            "{}-{}-{run_id}",
            spec.component.as_str(),
            std::process::id()
        );
        let ready_file = spec.runtime_dir.join(format!("{prefix}.ready.json"));
        let stop_file = spec.runtime_dir.join(format!("{prefix}.stop"));
        let _ = fs::remove_file(&ready_file);
        let _ = fs::remove_file(&stop_file);

        let mut command = Command::new(&spec.binary_path);
        command.args(&spec.args);
        command.env(ENV_SUPERVISED, "1");
        command.env(ENV_COMPONENT, spec.component.as_str());
        command.env(ENV_READY_FILE, &ready_file);
        command.env(ENV_STOP_FILE, &stop_file);
        if let Some(version) = spec.version {
            command.env(ENV_ACTIVE_VERSION, version.to_string());
        } else {
            command.env_remove(ENV_ACTIVE_VERSION);
        }
        for (key, value) in &spec.extra_env {
            command.env(key, value);
        }
        configure_platform_command(&mut command, spec.hide_console_window);

        let mut child = command.spawn().map_err(|source| ProcessError::Spawn {
            path: spec.binary_path.clone(),
            source,
        })?;
        let pid = child.id();

        #[cfg(windows)]
        let job_guard = match WindowsJobGuard::attach(&child) {
            Ok(guard) => guard,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };

        Ok(Self {
            child,
            pid,
            ready_file,
            stop_file,
            last_status: None,
            #[cfg(unix)]
            group_killed: AtomicBool::new(false),
            #[cfg(windows)]
            _job_guard: job_guard,
        })
    }

    /// Returns the operating system process identifier of the child.
    #[must_use]
    pub const fn pid(&self) -> u32 {
        self.pid
    }

    /// Checks whether the child has exited without blocking.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::Wait`] if querying OS child status fails.
    pub fn try_wait(&mut self) -> Result<Option<ChildExitStatus>, ProcessError> {
        if let Some(status) = self.last_status {
            return Ok(Some(status));
        }
        match self.child.try_wait() {
            Ok(Some(raw)) => {
                let normalized = ChildExitStatus::from_exit_status(raw);
                self.last_status = Some(normalized);
                self.cleanup_after_exit();
                Ok(Some(normalized))
            }
            Ok(None) => Ok(None),
            Err(source) => Err(ProcessError::Wait { source }),
        }
    }

    /// Returns true once the child writes a valid PID-matched readiness marker file.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        check_ready_marker(&self.ready_file, self.pid)
    }

    /// Requests graceful shutdown and forces termination if `stop_grace_ms` expires.
    ///
    /// Writes the stop sentinel file first so KeepPeek finalizes recordings cleanly.
    /// On Unix, sends `SIGTERM` to the leader PID after half the grace window if it
    /// has not exited yet, then escalates to process-group `SIGKILL` at the deadline.
    pub fn stop_and_wait(&mut self, stop_grace_ms: u64) -> ChildExitStatus {
        if let Some(status) = self.last_status {
            self.cleanup_after_exit();
            return status;
        }
        self.write_stop_sentinel();
        let total_grace = Duration::from_millis(stop_grace_ms.max(1));
        let started = Instant::now();
        let deadline = started + total_grace;
        let term_at = started + total_grace / 2;
        #[cfg(unix)]
        let mut sent_leader_term = false;

        while Instant::now() < deadline {
            if let Ok(Some(status)) = self.try_wait() {
                return status;
            }
            #[cfg(unix)]
            if !sent_leader_term && Instant::now() >= term_at {
                send_unix_leader_signal(self.pid, rustix::process::Signal::TERM);
                sent_leader_term = true;
            }
            #[cfg(not(unix))]
            let _ = term_at;
            thread::sleep(Duration::from_millis(STOP_POLL_INTERVAL_MS));
        }
        self.force_kill()
    }

    /// Immediately terminates the child process subtree and waits for OS reaping.
    pub fn force_kill(&mut self) -> ChildExitStatus {
        if let Some(status) = self.last_status {
            self.cleanup_after_exit();
            return status;
        }
        #[cfg(unix)]
        if !self.group_killed.swap(true, Ordering::AcqRel) {
            send_unix_group_signal(self.pid, rustix::process::Signal::KILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.last_status = Some(ChildExitStatus::Terminated);
        self.cleanup_after_exit();
        ChildExitStatus::Terminated
    }

    fn write_stop_sentinel(&self) {
        if let Err(error) = fs::write(&self.stop_file, b"stop\n") {
            warn!(
                pid = self.pid,
                path = %self.stop_file.display(),
                %error,
                "unable to write supervisor stop sentinel"
            );
        }
    }

    fn cleanup_after_exit(&self) {
        #[cfg(unix)]
        if !self.group_killed.swap(true, Ordering::AcqRel) {
            send_unix_group_signal(self.pid, rustix::process::Signal::KILL);
        }
        let _ = fs::remove_file(&self.ready_file);
        let _ = fs::remove_file(&self.stop_file);
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        if self.last_status.is_none() {
            let _ = self.force_kill();
        } else {
            self.cleanup_after_exit();
        }
    }
}

fn check_ready_marker(path: &Path, expected_pid: u32) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if metadata.len() == 0 || metadata.len() > MAX_READY_FILE_BYTES {
        return false;
    }
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    let Ok(marker) = serde_json::from_slice::<ReadyMarker>(&bytes) else {
        return false;
    };
    marker.ready && marker.pid == expected_pid
}

#[cfg(windows)]
fn configure_platform_command(command: &mut Command, hide_console_window: bool) {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    if hide_console_window {
        command.creation_flags(CREATE_NO_WINDOW);
    }
}

#[cfg(unix)]
fn configure_platform_command(command: &mut Command, _hide_console_window: bool) {
    use std::os::unix::process::CommandExt;

    command.process_group(0);
}

#[cfg(unix)]
fn send_unix_leader_signal(pid: u32, signal: rustix::process::Signal) {
    let Ok(raw_pid) = i32::try_from(pid) else {
        return;
    };
    if let Some(target) = rustix::process::Pid::from_raw(raw_pid) {
        let _ = rustix::process::kill_process(target, signal);
    }
}

#[cfg(unix)]
fn send_unix_group_signal(pid: u32, signal: rustix::process::Signal) {
    let Ok(raw_pid) = i32::try_from(pid) else {
        return;
    };
    if let Some(target) = rustix::process::Pid::from_raw(raw_pid) {
        let _ = rustix::process::kill_process_group(target, signal);
    }
}

#[cfg(windows)]
struct WindowsJobGuard {
    handle: windows::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl WindowsJobGuard {
    // ponytail: attaches the job object immediately after Command::spawn rather than
    // constructing PROC_THREAD_ATTRIBUTE_JOB_LIST; upgrade to CreateProcessW attribute
    // lists if KeepPeek ever spawns long-lived grandchildren during initial process boot.
    fn attach(child: &Child) -> Result<Self, ProcessError> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        // SAFETY: Passing null security attributes and null name creates an
        // anonymous Win32 Job Object owned by the calling process.
        let handle =
            unsafe { CreateJobObjectW(None, None) }.map_err(|error| ProcessError::JobObject {
                detail: format!("CreateJobObjectW failed: {error}"),
            })?;

        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let limits_size =
            u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                .expect("JOBOBJECT_EXTENDED_LIMIT_INFORMATION fits u32");

        // SAFETY: `handle` is a valid Job Object handle and `limits` points to
        // an initialized `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` of `limits_size` bytes.
        let set_result = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                limits_size,
            )
        };
        if let Err(error) = set_result {
            // SAFETY: `handle` was created by `CreateJobObjectW` and has not been closed.
            let _ = unsafe { CloseHandle(handle) };
            return Err(ProcessError::JobObject {
                detail: format!("SetInformationJobObject failed: {error}"),
            });
        }

        let child_handle = HANDLE(child.as_raw_handle());
        // SAFETY: `handle` is a valid Job Object and `child_handle` is a live
        // process handle owned by `child`.
        if let Err(error) = unsafe { AssignProcessToJobObject(handle, child_handle) } {
            // SAFETY: `handle` was created by `CreateJobObjectW` and has not been closed.
            let _ = unsafe { CloseHandle(handle) };
            return Err(ProcessError::JobObject {
                detail: format!("AssignProcessToJobObject failed: {error}"),
            });
        }

        Ok(Self { handle })
    }
}

#[cfg(windows)]
impl Drop for WindowsJobGuard {
    fn drop(&mut self) {
        // SAFETY: `self.handle` is an owned Win32 Job Object handle closed once on drop.
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.handle) };
    }
}
