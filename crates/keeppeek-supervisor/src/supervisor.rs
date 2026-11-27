//! Embeddable parent supervisor controller shared by CLI, SCM service, and Tauri hosts.
//!
//! Each enabled component (`keeppeek` core and optional `keeppeek-vision`) runs
//! on its own independent worker thread so Vision crashes, restarts, or upgrades
//! never interrupt core NVR recording.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::manifest::ComponentKind;
use crate::process::{ChildExitStatus, ChildSpawnSpec, ManagedChild, cleanup_stale_sentinels};
use crate::slots::{LaunchPlan, SlotError, SlotStore};
use crate::version::ReleaseVersion;

/// Default maximum wait for a child executable to signal readiness (30 seconds).
pub const DEFAULT_READINESS_TIMEOUT_MS: u64 = 30_000;

/// Default probation window after readiness before committing a pending upgrade (5 seconds).
pub const DEFAULT_PROBATION_MS: u64 = 5_000;

/// Default graceful shutdown wait before terminating a child process (15 seconds).
pub const DEFAULT_STOP_GRACE_MS: u64 = 15_000;

/// Default delay before respawning a crashed non-pending child process (1 second).
pub const DEFAULT_RESTART_BACKOFF_MS: u64 = 1_000;

/// Default supervisor control-loop polling cadence (50 milliseconds).
pub const DEFAULT_POLL_INTERVAL_MS: u64 = 50;

/// Default crash-loop exit threshold within the crash-loop window.
pub const DEFAULT_CRASH_LOOP_MAX_EXITS: u32 = 3;

/// Default rolling crash-loop window duration (180 seconds, covering three 30-second readiness timeouts).
pub const DEFAULT_CRASH_LOOP_WINDOW_MS: u64 = 180_000;

/// Maximum consecutive child-initiated immediate restarts (exit code 75) allowed inside the crash-loop window.
pub const MAX_IMMEDIATE_RESTARTS_IN_WINDOW: u32 = 5;

/// Supervision settings for one managed child executable component.
#[derive(Debug, Clone)]
pub struct ComponentConfig {
    /// Component managed by this worker.
    pub component: ComponentKind,
    /// Unmanaged fallback executable used when no A/B version is staged yet.
    pub fallback_binary: Option<PathBuf>,
    /// Arguments passed to the child executable on every spawn.
    pub args: Vec<OsString>,
    /// Additional environment variables supplied to the child process.
    pub extra_env: Vec<(String, String)>,
    /// Maximum time to wait for child readiness before treating boot as failed.
    pub readiness_timeout_ms: u64,
    /// Healthy uptime required after readiness before promoting a `pending` slot.
    pub probation_ms: u64,
    /// Maximum graceful drain duration before forced child termination.
    pub stop_grace_ms: u64,
    /// Delay before restarting an unexpected exit of a stable `active` child.
    pub restart_backoff_ms: u64,
    /// Maximum unexpected exits allowed within `crash_loop_window_ms` before halting.
    pub crash_loop_max_exits: u32,
    /// Rolling time window for crash-loop detection.
    pub crash_loop_window_ms: u64,
    /// Suppresses console windows when spawning child executables on Windows.
    pub hide_console_window: bool,
}

impl ComponentConfig {
    /// Creates default component supervision settings for `component`.
    #[must_use]
    pub const fn new(component: ComponentKind) -> Self {
        Self {
            component,
            fallback_binary: None,
            args: Vec::new(),
            extra_env: Vec::new(),
            readiness_timeout_ms: DEFAULT_READINESS_TIMEOUT_MS,
            probation_ms: DEFAULT_PROBATION_MS,
            stop_grace_ms: DEFAULT_STOP_GRACE_MS,
            restart_backoff_ms: DEFAULT_RESTART_BACKOFF_MS,
            crash_loop_max_exits: DEFAULT_CRASH_LOOP_MAX_EXITS,
            crash_loop_window_ms: DEFAULT_CRASH_LOOP_WINDOW_MS,
            hide_console_window: false,
        }
    }
}

/// Top-level configuration passed to [`SupervisorHandle::start`].
#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    /// Root directory containing `supervisor-state.json`, `versions/`, `vision/`, and `runtime/`.
    pub root: PathBuf,
    /// Core `keeppeek` child supervision settings.
    pub core: ComponentConfig,
    /// Optional independent `keeppeek-vision` child supervision settings.
    pub vision: Option<ComponentConfig>,
    /// Polling cadence for child exit and readiness checks.
    pub poll_interval_ms: u64,
}

impl SupervisorConfig {
    /// Creates a supervisor configuration rooted at `root` for core supervision only.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            core: ComponentConfig::new(ComponentKind::Core),
            vision: None,
            poll_interval_ms: DEFAULT_POLL_INTERVAL_MS,
        }
    }
}

/// Lifecycle phase reported for one supervised component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentPhase {
    /// Component worker is not running.
    Stopped,
    /// Child process spawned and waiting for readiness signal.
    Starting,
    /// Pending upgrade signaled readiness and is completing its probation window.
    Probation,
    /// Child process is healthy and running.
    Running,
    /// Child is draining for a requested restart or staged upgrade cutover.
    Restarting,
    /// Pending upgrade failed and is rolling back to the previous known-good slot.
    RollingBack,
    /// Component entered a terminal failure state (for example crash loop or missing binary).
    Failed,
}

/// Point-in-time status snapshot for one supervised component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentSnapshot {
    /// Supervised component identifier.
    pub component: ComponentKind,
    /// Current lifecycle phase.
    pub phase: ComponentPhase,
    /// Operating system process identifier of the live child, if running.
    pub pid: Option<u32>,
    /// Version of the currently running child, or `None` for fallback binaries.
    pub running_version: Option<ReleaseVersion>,
    /// Currently promoted active version in `supervisor-state.json`.
    pub active_version: Option<ReleaseVersion>,
    /// Last known-good previous version recorded in `supervisor-state.json`.
    pub previous_version: Option<ReleaseVersion>,
    /// Pending version undergoing or awaiting probation.
    pub pending_version: Option<ReleaseVersion>,
    /// Total restarts performed by this supervisor instance.
    pub restart_count: u64,
    /// Total automatic rollbacks performed by this supervisor instance.
    pub rollback_count: u64,
    /// Most recent error message recorded for this component, if any.
    pub last_error: Option<String>,
}

impl ComponentSnapshot {
    const fn initial(component: ComponentKind) -> Self {
        Self {
            component,
            phase: ComponentPhase::Stopped,
            pid: None,
            running_version: None,
            active_version: None,
            previous_version: None,
            pending_version: None,
            restart_count: 0,
            rollback_count: 0,
            last_error: None,
        }
    }
}

/// Combined status snapshot for all components managed by a [`SupervisorHandle`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorSnapshot {
    /// Supervisor root directory path.
    pub root: String,
    /// Core `keeppeek` component status.
    pub core: ComponentSnapshot,
    /// Optional `keeppeek-vision` component status.
    pub vision: Option<ComponentSnapshot>,
}

/// Errors returned while starting or waiting on a [`SupervisorHandle`].
#[derive(Debug)]
pub enum SupervisorError {
    /// Supervisor state could not be loaded before launch.
    Slot(SlotError),
    /// Requested component is not enabled in this supervisor instance.
    ComponentNotEnabled(ComponentKind),
    /// Component entered [`ComponentPhase::Failed`] while waiting for readiness.
    ComponentFailed {
        component: ComponentKind,
        detail: String,
    },
    /// Timed out waiting for a component to reach [`ComponentPhase::Running`].
    WaitTimedOut {
        component: ComponentKind,
        timeout_ms: u64,
    },
    /// Spawning the internal supervisor worker thread failed.
    ThreadSpawn(std::io::Error),
}

impl fmt::Display for SupervisorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Slot(error) => error.fmt(formatter),
            Self::ComponentNotEnabled(component) => {
                write!(
                    formatter,
                    "component `{component}` is not enabled in this supervisor"
                )
            }
            Self::ComponentFailed { component, detail } => {
                write!(formatter, "component `{component}` failed: {detail}")
            }
            Self::WaitTimedOut {
                component,
                timeout_ms,
            } => write!(
                formatter,
                "component `{component}` did not reach running state within {timeout_ms} ms"
            ),
            Self::ThreadSpawn(error) => {
                write!(
                    formatter,
                    "unable to spawn supervisor worker thread: {error}"
                )
            }
        }
    }
}

impl std::error::Error for SupervisorError {}

impl From<SlotError> for SupervisorError {
    fn from(error: SlotError) -> Self {
        Self::Slot(error)
    }
}

struct SharedWorkerState {
    stop_requested: AtomicBool,
    restart_requested: AtomicBool,
    snapshot: Mutex<ComponentSnapshot>,
    changed: Condvar,
}

impl SharedWorkerState {
    const fn new(component: ComponentKind) -> Self {
        Self {
            stop_requested: AtomicBool::new(false),
            restart_requested: AtomicBool::new(false),
            snapshot: Mutex::new(ComponentSnapshot::initial(component)),
            changed: Condvar::new(),
        }
    }

    fn update(&self, mutate: impl FnOnce(&mut ComponentSnapshot)) {
        let mut guard = self
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        mutate(&mut guard);
        self.changed.notify_all();
    }

    fn signal_stop(&self) {
        let _guard = self
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.stop_requested.store(true, Ordering::Release);
        self.changed.notify_all();
    }

    fn signal_restart(&self) {
        let _guard = self
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.restart_requested.store(true, Ordering::Release);
        self.changed.notify_all();
    }

    fn snapshot(&self) -> ComponentSnapshot {
        self.snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

struct ComponentWorkerHandle {
    shared: Arc<SharedWorkerState>,
    join_handle: Option<JoinHandle<()>>,
}

impl ComponentWorkerHandle {
    fn request_stop(&self) {
        self.shared.signal_stop();
    }

    fn join(&mut self) {
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

/// Active handle owning background supervision threads for `keeppeek` and optional `keeppeek-vision`.
pub struct SupervisorHandle {
    root: PathBuf,
    core: ComponentWorkerHandle,
    vision: Option<ComponentWorkerHandle>,
}

impl SupervisorHandle {
    /// Starts supervising configured components under `config`.
    ///
    /// # Errors
    ///
    /// Returns [`SupervisorError`] if initial state validation or worker thread
    /// creation fails.
    pub fn start(config: SupervisorConfig) -> Result<Self, SupervisorError> {
        let store = SlotStore::new(&config.root);
        let initial_state = store.load_state()?;
        let runtime_dir = config.root.join("runtime");
        cleanup_stale_sentinels(&runtime_dir, ComponentKind::Core);
        store.cleanup_stale_staging_dirs(ComponentKind::Core);
        let poll_interval = Duration::from_millis(config.poll_interval_ms.clamp(10, 500));

        let core = spawn_component_worker(
            store.clone(),
            config.core,
            poll_interval,
            initial_state.core.clone(),
        )?;
        let vision = match config.vision {
            Some(vision_config) => {
                cleanup_stale_sentinels(&runtime_dir, ComponentKind::Vision);
                store.cleanup_stale_staging_dirs(ComponentKind::Vision);
                Some(spawn_component_worker(
                    store,
                    vision_config,
                    poll_interval,
                    initial_state.vision,
                )?)
            }
            None => None,
        };

        Ok(Self {
            root: config.root,
            core,
            vision,
        })
    }

    /// Returns a current status snapshot across all supervised components.
    #[must_use]
    pub fn snapshot(&self) -> SupervisorSnapshot {
        SupervisorSnapshot {
            root: self.root.display().to_string(),
            core: self.core.shared.snapshot(),
            vision: self.vision.as_ref().map(|worker| worker.shared.snapshot()),
        }
    }

    /// Requests a clean restart of `component` so newly staged slots activate.
    ///
    /// Returns false when `component` is not enabled in this supervisor.
    #[must_use]
    pub fn request_restart(&self, component: ComponentKind) -> bool {
        let Some(shared) = self.shared_for(component) else {
            return false;
        };
        shared.signal_restart();
        true
    }

    /// Blocks until `component` reaches [`ComponentPhase::Running`] or fails.
    ///
    /// # Errors
    ///
    /// Returns [`SupervisorError`] if the component is disabled, enters
    /// [`ComponentPhase::Failed`], or does not become healthy within `timeout_ms`.
    pub fn wait_until_running(
        &self,
        component: ComponentKind,
        timeout_ms: u64,
    ) -> Result<ComponentSnapshot, SupervisorError> {
        self.wait_for_condition(component, timeout_ms, |snap| {
            snap.phase == ComponentPhase::Running
        })
    }

    /// Blocks until `component` finishes a restart beyond `prior_restart_count` and runs.
    ///
    /// # Errors
    ///
    /// Returns [`SupervisorError`] if the component is disabled, fails, or times out.
    pub fn wait_until_restarted_running(
        &self,
        component: ComponentKind,
        prior_restart_count: u64,
        timeout_ms: u64,
    ) -> Result<ComponentSnapshot, SupervisorError> {
        self.wait_for_condition(component, timeout_ms, |snap| {
            snap.phase == ComponentPhase::Running && snap.restart_count > prior_restart_count
        })
    }

    /// Gracefully stops all supervised child processes in parallel and joins worker threads.
    pub fn stop(&mut self) {
        self.core.request_stop();
        if let Some(vision) = self.vision.as_ref() {
            vision.request_stop();
        }
        if let Some(vision) = self.vision.as_mut() {
            vision.join();
        }
        self.core.join();
    }

    fn wait_for_condition(
        &self,
        component: ComponentKind,
        timeout_ms: u64,
        predicate: impl Fn(&ComponentSnapshot) -> bool,
    ) -> Result<ComponentSnapshot, SupervisorError> {
        let Some(shared) = self.shared_for(component) else {
            return Err(SupervisorError::ComponentNotEnabled(component));
        };
        let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(1));
        let mut guard = shared
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if predicate(&guard) {
                return Ok(guard.clone());
            }
            if guard.phase == ComponentPhase::Failed {
                return Err(SupervisorError::ComponentFailed {
                    component,
                    detail: guard
                        .last_error
                        .clone()
                        .unwrap_or_else(|| "component stopped unexpectedly".to_owned()),
                });
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(SupervisorError::WaitTimedOut {
                    component,
                    timeout_ms,
                });
            }
            let wait_for = deadline.saturating_duration_since(now);
            let (next_guard, _) = shared
                .changed
                .wait_timeout(guard, wait_for)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard = next_guard;
        }
    }

    fn shared_for(&self, component: ComponentKind) -> Option<&Arc<SharedWorkerState>> {
        match component {
            ComponentKind::Core => Some(&self.core.shared),
            ComponentKind::Vision => self.vision.as_ref().map(|worker| &worker.shared),
        }
    }
}

impl Drop for SupervisorHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

fn spawn_component_worker(
    store: SlotStore,
    config: ComponentConfig,
    poll_interval: Duration,
    initial_slot: crate::slots::ComponentSlotState,
) -> Result<ComponentWorkerHandle, SupervisorError> {
    let shared = Arc::new(SharedWorkerState::new(config.component));
    shared.update(|snapshot| {
        snapshot.active_version = initial_slot.active;
        snapshot.previous_version = initial_slot.previous;
        snapshot.pending_version = initial_slot.pending.as_ref().map(|item| item.version);
    });
    let worker_shared = Arc::clone(&shared);
    let thread_name = format!("supervisor-{}", config.component.as_str());
    let join_handle = thread::Builder::new()
        .name(thread_name)
        .spawn(move || run_component_loop(store, config, poll_interval, worker_shared))
        .map_err(SupervisorError::ThreadSpawn)?;
    Ok(ComponentWorkerHandle {
        shared,
        join_handle: Some(join_handle),
    })
}

enum CycleOutcome {
    Stop,
    RestartRequestedByHost,
    RestartRequestedByChild,
    BackoffThenRestart,
    TerminalFailure(String),
}

fn run_component_loop(
    store: SlotStore,
    config: ComponentConfig,
    poll_interval: Duration,
    shared: Arc<SharedWorkerState>,
) {
    let mut crash_timestamps: VecDeque<Instant> = VecDeque::new();
    let mut immediate_restart_timestamps: VecDeque<Instant> = VecDeque::new();
    let mut launches: u64 = 0;

    // This loop runs until a stop request or a terminal failure occurs; each iteration bounds one child lifecycle.
    while !shared.stop_requested.load(Ordering::Acquire) {
        shared.restart_requested.store(false, Ordering::Release);
        let plan = match store.prepare_launch(config.component, config.fallback_binary.as_deref()) {
            Ok(plan) => plan,
            Err(error @ (SlotError::Io { .. } | SlotError::SlotIntegrityFailed { .. })) => {
                warn!(
                    component = config.component.as_str(),
                    %error,
                    "transient slot launch preparation error; backing off before retry"
                );
                shared.update(|snapshot| snapshot.last_error = Some(error.to_string()));
                if record_window_exceeds_limit(
                    &mut crash_timestamps,
                    config.crash_loop_max_exits,
                    config.crash_loop_window_ms,
                ) {
                    mark_failed(&shared, &store, config.component, error.to_string());
                    return;
                }
                sleep_until_stopped(&shared, Duration::from_millis(config.restart_backoff_ms));
                continue;
            }
            Err(error) => {
                mark_failed(&shared, &store, config.component, error.to_string());
                return;
            }
        };
        if launches > 0 {
            shared.update(|snapshot| {
                snapshot.restart_count = snapshot.restart_count.saturating_add(1);
            });
        }
        launches = launches.saturating_add(1);

        let outcome = run_single_child(&store, &config, &plan, poll_interval, &shared);
        match outcome {
            CycleOutcome::Stop => break,
            CycleOutcome::RestartRequestedByHost => {}
            CycleOutcome::RestartRequestedByChild => {
                if record_window_exceeds_limit(
                    &mut immediate_restart_timestamps,
                    MAX_IMMEDIATE_RESTARTS_IN_WINDOW,
                    config.crash_loop_window_ms,
                ) {
                    let message = format!(
                        "{} exceeded {MAX_IMMEDIATE_RESTARTS_IN_WINDOW} rapid child restarts within {} ms",
                        config.component, config.crash_loop_window_ms
                    );
                    mark_failed(&shared, &store, config.component, message);
                    return;
                }
            }
            CycleOutcome::BackoffThenRestart => {
                if record_window_exceeds_limit(
                    &mut crash_timestamps,
                    config.crash_loop_max_exits,
                    config.crash_loop_window_ms,
                ) {
                    let message = format!(
                        "{} exceeded {} unexpected exits within {} ms",
                        config.component, config.crash_loop_max_exits, config.crash_loop_window_ms
                    );
                    mark_failed(&shared, &store, config.component, message);
                    return;
                }
                sleep_until_stopped(&shared, Duration::from_millis(config.restart_backoff_ms));
            }
            CycleOutcome::TerminalFailure(detail) => {
                mark_failed(&shared, &store, config.component, detail);
                return;
            }
        }
    }

    refresh_slot_pointers(&shared, &store, config.component);
    shared.update(|snapshot| {
        snapshot.phase = ComponentPhase::Stopped;
        snapshot.pid = None;
        snapshot.running_version = None;
    });
}

fn run_single_child(
    store: &SlotStore,
    config: &ComponentConfig,
    plan: &LaunchPlan,
    poll_interval: Duration,
    shared: &Arc<SharedWorkerState>,
) -> CycleOutcome {
    let spec = ChildSpawnSpec {
        component: plan.component,
        binary_path: plan.binary_path.clone(),
        version: plan.version,
        args: config.args.clone(),
        extra_env: config.extra_env.clone(),
        runtime_dir: store.root().join("runtime"),
        hide_console_window: config.hide_console_window,
    };
    let mut child = match ManagedChild::spawn(&spec) {
        Ok(child) => child,
        Err(error) => {
            return handle_pending_or_active_failure(store, plan, shared, error.to_string());
        }
    };
    record_child_started(store, plan, child.pid(), shared);
    monitor_child_until_exit(store, config, plan, &mut child, poll_interval, shared)
}

fn record_child_started(
    store: &SlotStore,
    plan: &LaunchPlan,
    pid: u32,
    shared: &Arc<SharedWorkerState>,
) {
    info!(
        component = plan.component.as_str(),
        pid,
        version = ?plan.version,
        pending = plan.is_pending_probation,
        "launched supervised child executable"
    );
    refresh_slot_pointers(shared, store, plan.component);
    shared.update(|snapshot| {
        snapshot.phase = ComponentPhase::Starting;
        snapshot.pid = Some(pid);
        snapshot.running_version = plan.version;
        snapshot.last_error = None;
    });
}

struct ChildLifecycleState {
    readiness_deadline: Instant,
    ready_at: Option<Instant>,
    pending_committed: bool,
}

fn monitor_child_until_exit(
    store: &SlotStore,
    config: &ComponentConfig,
    plan: &LaunchPlan,
    child: &mut ManagedChild,
    poll_interval: Duration,
    shared: &Arc<SharedWorkerState>,
) -> CycleOutcome {
    let mut lifecycle = ChildLifecycleState {
        readiness_deadline: Instant::now()
            + Duration::from_millis(config.readiness_timeout_ms.max(1)),
        ready_at: None,
        pending_committed: !plan.is_pending_probation,
    };

    loop {
        if let Some(outcome) = poll_control_and_exit(
            store,
            config,
            plan,
            child,
            lifecycle.pending_committed,
            shared,
        ) {
            return outcome;
        }
        if let Some(outcome) =
            advance_readiness_and_probation(store, config, plan, child, &mut lifecycle, shared)
        {
            return outcome;
        }
        thread::sleep(poll_interval);
    }
}

fn poll_control_and_exit(
    store: &SlotStore,
    config: &ComponentConfig,
    plan: &LaunchPlan,
    child: &mut ManagedChild,
    pending_committed: bool,
    shared: &Arc<SharedWorkerState>,
) -> Option<CycleOutcome> {
    if shared.stop_requested.load(Ordering::Acquire) {
        shared.update(|snapshot| snapshot.phase = ComponentPhase::Stopped);
        let _ = child.stop_and_wait(config.stop_grace_ms);
        return Some(CycleOutcome::Stop);
    }
    if shared.restart_requested.swap(false, Ordering::AcqRel) {
        shared.update(|snapshot| snapshot.phase = ComponentPhase::Restarting);
        let _ = child.stop_and_wait(config.stop_grace_ms);
        return Some(CycleOutcome::RestartRequestedByHost);
    }
    match child.try_wait() {
        Ok(Some(status)) => Some(handle_child_exit(
            store,
            plan,
            status,
            pending_committed,
            shared,
        )),
        Ok(None) => None,
        Err(error) => {
            let _ = child.force_kill();
            Some(handle_pending_or_active_failure(
                store,
                plan,
                shared,
                error.to_string(),
            ))
        }
    }
}

fn advance_readiness_and_probation(
    store: &SlotStore,
    config: &ComponentConfig,
    plan: &LaunchPlan,
    child: &mut ManagedChild,
    lifecycle: &mut ChildLifecycleState,
    shared: &Arc<SharedWorkerState>,
) -> Option<CycleOutcome> {
    if lifecycle.ready_at.is_none() {
        if child.is_ready() {
            lifecycle.ready_at = Some(Instant::now());
            let next_phase = if plan.is_pending_probation {
                ComponentPhase::Probation
            } else {
                ComponentPhase::Running
            };
            shared.update(|snapshot| snapshot.phase = next_phase);
        } else if Instant::now() >= lifecycle.readiness_deadline {
            let detail = format!(
                "child PID {} did not signal readiness within {} ms",
                child.pid(),
                config.readiness_timeout_ms
            );
            warn!(
                component = plan.component.as_str(),
                pid = child.pid(),
                "{detail}"
            );
            let _ = child.stop_and_wait(config.stop_grace_ms);
            return Some(handle_pending_or_active_failure(
                store, plan, shared, detail,
            ));
        }
    }
    if let Some(became_ready) = lifecycle.ready_at
        && !lifecycle.pending_committed
        && became_ready.elapsed() >= Duration::from_millis(config.probation_ms)
    {
        if let Some(version) = plan.version {
            if let Err(error) = store.commit_healthy(plan.component, version) {
                let _ = child.stop_and_wait(config.stop_grace_ms);
                return Some(CycleOutcome::TerminalFailure(error.to_string()));
            }
            info!(
                component = plan.component.as_str(),
                %version,
                "committed healthy pending release as active"
            );
        }
        lifecycle.pending_committed = true;
        refresh_slot_pointers(shared, store, plan.component);
        shared.update(|snapshot| snapshot.phase = ComponentPhase::Running);
    }
    None
}

fn handle_child_exit(
    store: &SlotStore,
    plan: &LaunchPlan,
    status: ChildExitStatus,
    pending_committed: bool,
    shared: &Arc<SharedWorkerState>,
) -> CycleOutcome {
    if status == ChildExitStatus::RestartRequested {
        info!(
            component = plan.component.as_str(),
            "child requested clean supervised restart"
        );
        shared.update(|snapshot| {
            snapshot.phase = ComponentPhase::Restarting;
            snapshot.pid = None;
        });
        return CycleOutcome::RestartRequestedByChild;
    }
    if status == ChildExitStatus::CleanExit && pending_committed {
        info!(
            component = plan.component.as_str(),
            "supervised child exited cleanly"
        );
        return CycleOutcome::Stop;
    }
    let detail = format!("child exited before completing lifecycle: {status:?}");
    if !pending_committed {
        return handle_pending_or_active_failure(store, plan, shared, detail);
    }
    warn!(component = plan.component.as_str(), "{detail}");
    shared.update(|snapshot| {
        snapshot.pid = None;
        snapshot.last_error = Some(detail);
    });
    CycleOutcome::BackoffThenRestart
}

fn handle_pending_or_active_failure(
    store: &SlotStore,
    plan: &LaunchPlan,
    shared: &Arc<SharedWorkerState>,
    detail: String,
) -> CycleOutcome {
    if plan.is_pending_probation {
        shared.update(|snapshot| {
            snapshot.phase = ComponentPhase::RollingBack;
            snapshot.pid = None;
            snapshot.last_error = Some(detail.clone());
        });
        match store.rollback_failed_pending(plan.component) {
            Ok(Some(outcome)) => {
                warn!(
                    component = plan.component.as_str(),
                    quarantined = %outcome.quarantined_version,
                    fallback = ?outcome.fallback_active,
                    "rolled back failed pending release"
                );
                refresh_slot_pointers(shared, store, plan.component);
                shared.update(|snapshot| {
                    snapshot.rollback_count = snapshot.rollback_count.saturating_add(1);
                });
                return CycleOutcome::RestartRequestedByHost;
            }
            Ok(None) => return CycleOutcome::BackoffThenRestart,
            Err(error) => return CycleOutcome::TerminalFailure(error.to_string()),
        }
    }
    shared.update(|snapshot| {
        snapshot.pid = None;
        snapshot.last_error = Some(detail);
    });
    CycleOutcome::BackoffThenRestart
}

fn record_window_exceeds_limit(
    timestamps: &mut VecDeque<Instant>,
    max_events: u32,
    window_ms: u64,
) -> bool {
    let now = Instant::now();
    let window = Duration::from_millis(window_ms.max(1));
    while let Some(&oldest) = timestamps.front() {
        if now.saturating_duration_since(oldest) > window {
            timestamps.pop_front();
        } else {
            break;
        }
    }
    timestamps.push_back(now);
    let limit = usize::try_from(max_events.max(1)).unwrap_or(usize::MAX);
    timestamps.len() >= limit
}

fn refresh_slot_pointers(
    shared: &Arc<SharedWorkerState>,
    store: &SlotStore,
    component: ComponentKind,
) {
    if let Ok(state) = store.load_state() {
        let slot = state.component(component);
        shared.update(|snapshot| {
            snapshot.active_version = slot.active;
            snapshot.previous_version = slot.previous;
            snapshot.pending_version = slot.pending.as_ref().map(|item| item.version);
        });
    }
}

fn mark_failed(
    shared: &Arc<SharedWorkerState>,
    store: &SlotStore,
    component: ComponentKind,
    detail: String,
) {
    error!(component = component.as_str(), "{detail}");
    refresh_slot_pointers(shared, store, component);
    shared.update(|snapshot| {
        snapshot.phase = ComponentPhase::Failed;
        snapshot.pid = None;
        snapshot.running_version = None;
        snapshot.last_error = Some(detail);
    });
}

fn sleep_until_stopped(shared: &Arc<SharedWorkerState>, duration: Duration) {
    let deadline = Instant::now() + duration;
    let mut guard = shared
        .snapshot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while !shared.stop_requested.load(Ordering::Acquire) {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let (next_guard, _) = shared
            .changed
            .wait_timeout(guard, deadline.saturating_duration_since(now))
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard = next_guard;
    }
}
