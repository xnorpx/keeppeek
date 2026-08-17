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
    use std::{
        ffi::OsString,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    use keeppeek::{
        app, config,
        logging::initialize_service_logging,
        shutdown::{Restart, Shutdown},
    };
    use tracing::{error, info};
    use windows_service::{
        Result, define_windows_service,
        service::{
            ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
            ServiceType,
        },
        service_control_handler::{self, ServiceControlHandlerResult},
        service_dispatcher,
    };

    const SERVICE_NAME: &str = "KeepPeekService";
    const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

    pub fn run() -> Result<()> {
        service_dispatcher::start(SERVICE_NAME, ffi_service_main)
    }

    define_windows_service!(ffi_service_main, service_main);

    fn service_main(_arguments: Vec<OsString>) {
        let shutdown = Arc::new(Mutex::new(Shutdown::new()));
        let stopping = Arc::new(AtomicBool::new(false));
        let signal_shutdown = shutdown.clone();
        let signal_stopping = stopping.clone();
        let status_handle =
            match service_control_handler::register(SERVICE_NAME, move |control| match control {
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    signal_stopping.store(true, Ordering::Release);
                    signal_shutdown
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .cancel();
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                _ => ServiceControlHandlerResult::NotImplemented,
            }) {
                Ok(handle) => handle,
                Err(error) => {
                    eprintln!("unable to register KeepPeek service handler: {error}");
                    return;
                }
            };

        if let Err(error) = status_handle.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::StartPending,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::from_secs(60),
            process_id: None,
        }) {
            eprintln!("unable to report KeepPeek service startup: {error}");
            return;
        }

        let (mut config, mut config_path) = match config::load() {
            Ok(config) => config,
            Err(error) => {
                eprintln!("unable to load KeepPeek service configuration: {error}");
                stop_service(&status_handle, 1);
                return;
            }
        };
        let logging = match initialize_service_logging(&config_path, config.logging.service) {
            Ok(logging) => logging,
            Err(error) => {
                eprintln!("unable to initialize KeepPeek service logging: {error}");
                stop_service(&status_handle, 1);
                return;
            }
        };

        if let Err(error) = status_handle.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        }) {
            error!(%error, "unable to report KeepPeek service running");
            stopping.store(true, Ordering::Release);
            shutdown
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .cancel();
        }

        let mut exit_code = 0;
        loop {
            let app_shutdown = Shutdown::new();
            *shutdown
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = app_shutdown.clone();
            let restart = Restart::default();
            match app::run(config, &config_path, logging.clone(), app_shutdown, restart) {
                Ok(true) if !stopping.load(Ordering::Acquire) => {
                    info!("reloading configuration after restart request");
                    match config::load() {
                        Ok((next_config, next_config_path)) => {
                            config = next_config;
                            config_path = next_config_path;
                        }
                        Err(error) => {
                            error!(%error, "unable to reload KeepPeek service configuration");
                            exit_code = 1;
                            break;
                        }
                    }
                }
                Ok(_) => break,
                Err(error) => {
                    error!(%error, "KeepPeek service stopped with an error");
                    exit_code = 1;
                    break;
                }
            }
        }

        stop_service(&status_handle, exit_code);
    }

    fn stop_service(status_handle: &service_control_handler::ServiceStatusHandle, code: u32) {
        if let Err(error) = status_handle.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::Stopped,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(code),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        }) {
            eprintln!("unable to report KeepPeek service stopped: {error}");
        }
    }
}
