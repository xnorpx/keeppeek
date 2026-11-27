//! Opt-in local server supervision embedded in KeepPeek Desktop.
//!
//! Desktop remains connect-to-existing-server by default. Only when the user
//! explicitly opts in does this module embed [`SupervisorHandle`] in trusted
//! Rust code (requiring zero extra Tauri webview or plugin permissions).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use keeppeek_supervisor::manifest::ComponentKind;
use keeppeek_supervisor::slots::SlotStore;
use keeppeek_supervisor::supervisor::{
    ComponentConfig, ComponentSnapshot, SupervisorConfig, SupervisorError, SupervisorHandle,
    SupervisorSnapshot,
};

/// Default wait budget for an opt-in local server to become ready, including one rollback cycle (90 seconds).
pub const DEFAULT_LOCAL_READY_TIMEOUT_MS: u64 = 90_000;

/// Resolves the fallback `keeppeek` executable used before any A/B version slot is staged.
#[must_use]
pub fn resolve_local_server_binary(
    explicit_override: Option<&Path>,
    desktop_exe: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(path) = explicit_override {
        return Some(path.to_path_buf());
    }
    let sibling = desktop_exe?
        .parent()?
        .join(ComponentKind::Core.current_binary_file_name());
    sibling.is_file().then_some(sibling)
}

/// Builds the supervisor configuration used by KeepPeek Desktop's opt-in local server mode.
#[must_use]
pub fn build_desktop_supervisor_config(
    supervisor_root: &Path,
    config_path: &Path,
    server_binary_override: Option<&Path>,
    desktop_exe: Option<&Path>,
    allow_local: bool,
) -> SupervisorConfig {
    let mut config = SupervisorConfig::new(supervisor_root);
    config.launch_policy.allow_local = allow_local;
    config.enforce_root_acl = allow_local;
    config.core.fallback_binary = resolve_local_server_binary(server_binary_override, desktop_exe);
    config.core.args = vec![
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
        OsString::from("--tui"),
        OsString::from("never"),
    ];
    config.core.hide_console_window = true;

    let vision_fallback = desktop_exe
        .and_then(Path::parent)
        .map(|dir| dir.join(ComponentKind::Vision.current_binary_file_name()))
        .filter(|path| path.is_file());
    let has_vision_slot = SlotStore::new(supervisor_root)
        .load_state()
        .ok()
        .is_some_and(|state| state.vision.active.is_some() || state.vision.pending.is_some());
    if vision_fallback.is_some() || has_vision_slot {
        let mut vision = ComponentConfig::new(ComponentKind::Vision);
        vision.fallback_binary = vision_fallback;
        vision.hide_console_window = true;
        config.vision = Some(vision);
    }

    config
}

/// Active handle to a desktop-supervised local `keeppeek` child process.
pub struct ManagedLocalServer {
    handle: SupervisorHandle,
}

impl ManagedLocalServer {
    /// Starts supervising the local `keeppeek` child executable under `config`.
    ///
    /// # Errors
    ///
    /// Returns a [`SupervisorError`] if initial slot state loading or worker
    /// startup fails.
    pub fn start(config: SupervisorConfig) -> Result<Self, SupervisorError> {
        let handle = SupervisorHandle::start(config)?;
        Ok(Self { handle })
    }

    /// Waits until the supervised local `keeppeek` child signals readiness.
    ///
    /// # Errors
    ///
    /// Returns a [`SupervisorError`] if the child fails or times out.
    pub fn wait_ready(&self, timeout_ms: u64) -> Result<ComponentSnapshot, SupervisorError> {
        self.handle
            .wait_until_running(ComponentKind::Core, timeout_ms)
    }

    /// Returns the current supervisor status snapshot.
    #[must_use]
    pub fn snapshot(&self) -> SupervisorSnapshot {
        self.handle.snapshot()
    }

    /// Requests a graceful restart of the supervised local `keeppeek` child.
    #[must_use]
    pub fn restart_core(&self) -> bool {
        self.handle.request_restart(ComponentKind::Core)
    }

    /// Waits until the supervised child completes a restart beyond `prior_restart_count`.
    ///
    /// # Errors
    ///
    /// Returns a [`SupervisorError`] if the restarted child fails or times out.
    pub fn wait_restarted_ready(
        &self,
        prior_restart_count: u64,
        timeout_ms: u64,
    ) -> Result<ComponentSnapshot, SupervisorError> {
        self.handle.wait_until_restarted_running(
            ComponentKind::Core,
            prior_restart_count,
            timeout_ms,
        )
    }

    /// Stops all desktop-supervised child processes cleanly.
    pub fn stop(&mut self) {
        self.handle.stop();
    }
}
