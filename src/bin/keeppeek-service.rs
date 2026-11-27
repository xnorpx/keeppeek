#[cfg(windows)]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[cfg(windows)]
fn main() -> windows_service::Result<()> {
    service::run()
}

#[cfg(not(windows))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("keeppeek-service is only available on Windows")
}

#[cfg(windows)]
mod service {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use keeppeek::{config, logging::initialize_service_logging};
    use keeppeek_supervisor::manifest::ComponentKind;
    use keeppeek_supervisor::slots::SlotStore;
    use keeppeek_supervisor::supervisor::{
        ComponentConfig, ComponentPhase, SupervisorConfig, SupervisorHandle,
    };
    use tracing::{error, info};
    use windows_service::{
        Result, define_windows_service,
        service::{
            ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
            ServiceType,
        },
        service_control_handler::{self, ServiceControlHandlerResult, ServiceStatusHandle},
        service_dispatcher,
    };

    const SERVICE_NAME: &str = "KeepPeekService";
    const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;
    const STARTUP_WAIT_TIMEOUT_MS: u64 = 90_000;
    const SCM_CHECKPOINT_INTERVAL_MS: u64 = 2_000;
    const SCM_WAIT_HINT_SECS: u64 = 30;

    pub fn run() -> Result<()> {
        service_dispatcher::start(SERVICE_NAME, ffi_service_main)
    }

    define_windows_service!(ffi_service_main, service_main);

    fn service_main(_arguments: Vec<OsString>) {
        let stopping = Arc::new(AtomicBool::new(false));
        let Ok(status_handle) = register_handler(&stopping) else {
            return;
        };
        if set_status(
            &status_handle,
            ServiceState::StartPending,
            ServiceControlAccept::empty(),
            0,
            1,
            Duration::from_secs(SCM_WAIT_HINT_SECS),
        )
        .is_err()
        {
            return;
        }
        let exit_code = run_supervised_service(&status_handle, &stopping);
        stop_service(&status_handle, exit_code);
    }

    fn register_handler(stopping: &Arc<AtomicBool>) -> Result<ServiceStatusHandle> {
        let signal_stopping = Arc::clone(stopping);
        service_control_handler::register(SERVICE_NAME, move |control| match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                signal_stopping.store(true, Ordering::Release);
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        })
    }

    fn run_supervised_service(status_handle: &ServiceStatusHandle, stopping: &AtomicBool) -> u32 {
        let (service_config, config_path) = match config::load() {
            Ok(loaded) => loaded,
            Err(error) => {
                eprintln!("unable to load KeepPeek service configuration: {error}");
                return 1;
            }
        };
        let _logging =
            match initialize_service_logging(&config_path, service_config.logging.service) {
                Ok(logging) => logging,
                Err(error) => {
                    eprintln!("unable to initialize KeepPeek service logging: {error}");
                    return 1;
                }
            };

        let current_exe = std::env::current_exe().ok();
        let allow_local = std::env::args_os().any(|arg| arg == "--allow-local");
        let allow_unsigned_main = std::env::args_os().any(|arg| arg == "--allow-unsigned-main");
        let supervisor_config = build_service_supervisor_config(
            &config_path,
            current_exe.as_deref(),
            allow_local,
            allow_unsigned_main,
        );
        info!(
            root = %supervisor_config.root.display(),
            fallback = ?supervisor_config.core.fallback_binary,
            vision_enabled = supervisor_config.vision.is_some(),
            allow_local,
            allow_unsigned_main,
            "starting KeepPeek thin SCM supervisor"
        );
        let mut supervisor = match SupervisorHandle::start(supervisor_config) {
            Ok(handle) => handle,
            Err(error) => {
                error!(%error, "unable to start KeepPeek child supervisor");
                return 1;
            }
        };

        if !wait_for_initial_readiness(status_handle, &supervisor, stopping) {
            stop_supervisor_with_checkpoints(status_handle, &mut supervisor, 1);
            return if stopping.load(Ordering::Acquire) {
                0
            } else {
                1
            };
        }

        if let Err(error) = set_status(
            status_handle,
            ServiceState::Running,
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            0,
            0,
            Duration::default(),
        ) {
            error!(%error, "unable to report KeepPeek service running");
            stop_supervisor_with_checkpoints(status_handle, &mut supervisor, 1);
            return 1;
        }

        let exit_code = monitor_running_service(&supervisor, stopping);
        stop_supervisor_with_checkpoints(status_handle, &mut supervisor, exit_code);
        exit_code
    }

    fn wait_for_initial_readiness(
        status_handle: &ServiceStatusHandle,
        supervisor: &SupervisorHandle,
        stopping: &AtomicBool,
    ) -> bool {
        let deadline = Instant::now() + Duration::from_millis(STARTUP_WAIT_TIMEOUT_MS);
        let mut checkpoint: u32 = 2;
        while !stopping.load(Ordering::Acquire) {
            let core = supervisor.snapshot().core;
            match core.phase {
                ComponentPhase::Running => return true,
                ComponentPhase::Failed => {
                    error!(detail = ?core.last_error, "KeepPeek child failed during service startup");
                    return false;
                }
                _ => {}
            }
            if Instant::now() >= deadline {
                error!("KeepPeek child did not reach running state within startup timeout");
                return false;
            }
            let _ = set_status(
                status_handle,
                ServiceState::StartPending,
                ServiceControlAccept::empty(),
                0,
                checkpoint,
                Duration::from_secs(SCM_WAIT_HINT_SECS),
            );
            checkpoint = checkpoint.saturating_add(1);
            let _ = supervisor.wait_until_running(ComponentKind::Core, SCM_CHECKPOINT_INTERVAL_MS);
        }
        false
    }

    fn monitor_running_service(supervisor: &SupervisorHandle, stopping: &AtomicBool) -> u32 {
        while !stopping.load(Ordering::Acquire) {
            let core = supervisor.snapshot().core;
            if core.phase == ComponentPhase::Failed {
                error!(
                    detail = ?core.last_error,
                    "supervised KeepPeek child entered failed state"
                );
                return 1;
            }
            if core.phase == ComponentPhase::Stopped {
                return 0;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        0
    }

    fn stop_supervisor_with_checkpoints(
        status_handle: &ServiceStatusHandle,
        supervisor: &mut SupervisorHandle,
        exit_code: u32,
    ) {
        let _ = set_status(
            status_handle,
            ServiceState::StopPending,
            ServiceControlAccept::empty(),
            exit_code,
            1,
            Duration::from_secs(SCM_WAIT_HINT_SECS),
        );
        supervisor.stop();
    }

    fn build_service_supervisor_config(
        config_path: &Path,
        service_exe: Option<&Path>,
        allow_local: bool,
        allow_unsigned_main: bool,
    ) -> SupervisorConfig {
        let root = config_path
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .join("supervisor");
        let mut supervisor = SupervisorConfig::new(&root);
        supervisor.launch_policy.allow_local = allow_local;
        supervisor.launch_policy.allow_unsigned_main = allow_unsigned_main;
        supervisor.enforce_root_acl = true;
        supervisor.core.fallback_binary = resolve_sibling_binary(service_exe, ComponentKind::Core);
        supervisor.core.args = vec![
            OsString::from("--config"),
            config_path.as_os_str().to_os_string(),
            OsString::from("--tui"),
            OsString::from("never"),
        ];
        supervisor.core.hide_console_window = true;

        let vision_fallback = resolve_sibling_binary(service_exe, ComponentKind::Vision);
        let has_vision_slot = SlotStore::new(&root)
            .load_state()
            .ok()
            .is_some_and(|state| state.vision.active.is_some() || state.vision.pending.is_some());
        if vision_fallback.is_some() || has_vision_slot {
            let mut vision = ComponentConfig::new(ComponentKind::Vision);
            vision.fallback_binary = vision_fallback;
            vision.hide_console_window = true;
            supervisor.vision = Some(vision);
        }
        supervisor
    }

    fn resolve_sibling_binary(
        service_exe: Option<&Path>,
        component: ComponentKind,
    ) -> Option<PathBuf> {
        let sibling = service_exe?
            .parent()?
            .join(component.current_binary_file_name());
        sibling.is_file().then_some(sibling)
    }

    fn set_status(
        status_handle: &ServiceStatusHandle,
        current_state: ServiceState,
        controls_accepted: ServiceControlAccept,
        code: u32,
        checkpoint: u32,
        wait_hint: Duration,
    ) -> Result<()> {
        let exit_code = if code == 0 {
            ServiceExitCode::Win32(0)
        } else {
            ServiceExitCode::ServiceSpecific(code)
        };
        status_handle.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state,
            controls_accepted,
            exit_code,
            checkpoint,
            wait_hint,
            process_id: None,
        })
    }

    fn stop_service(status_handle: &ServiceStatusHandle, code: u32) {
        if let Err(error) = set_status(
            status_handle,
            ServiceState::Stopped,
            ServiceControlAccept::empty(),
            code,
            0,
            Duration::default(),
        ) {
            error!(%error, "unable to report KeepPeek service stopped");
        }
    }
}
