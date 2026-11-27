use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use keeppeek_desktop::{
    config_store::{self, ResolvedTarget, TargetSource},
    managed_server::{self, DEFAULT_LOCAL_READY_TIMEOUT_MS, ManagedLocalServer},
    navigation::{self, NavigationDecision},
    probe::{self, ProbeOptions, ServerProbeResult},
    server_url::ServerOrigin,
};
use keeppeek_supervisor::supervisor::SupervisorSnapshot;
use serde::Serialize;
#[cfg(target_os = "macos")]
use tauri::WindowEvent;
use tauri::{
    AppHandle, Manager, RunEvent, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
    menu::{Menu, MenuItem, PredefinedMenuItem, Submenu},
    webview::{NewWindowResponse, PageLoadEvent},
};
use tracing::{info, warn};
use url::Url;

const MAIN_WINDOW_LABEL: &str = "main";
const MENU_SWITCH_SERVER: &str = "connection_switch_server";
const MENU_RECONNECT: &str = "connection_reconnect";
const MENU_RELOAD: &str = "connection_reload";
const MENU_RESTART_LOCAL_SERVER: &str = "connection_restart_local_server";
const MENU_QUIT: &str = "connection_quit";
const MAX_SMOKE_EXIT_MS: u64 = 600_000;

/// Launch options passed from the CLI to the desktop runtime.
#[derive(Debug, Clone)]
pub struct LaunchOptions {
    pub target: ResolvedTarget,
    pub auto_connect: bool,
    pub server_bin: Option<PathBuf>,
    pub allow_local: bool,
    pub diagnostics_file: Option<PathBuf>,
    pub smoke_exit_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DesktopStateSnapshot {
    pub server_url: String,
    pub source: TargetSource,
    pub config_path: String,
    pub saved_server_url: Option<String>,
    pub active_origin: Option<String>,
    pub auto_connect: bool,
    pub manage_local_server: bool,
    pub supervisor_root: String,
    pub managed_server: Option<SupervisorSnapshot>,
    pub last_error: Option<String>,
    pub blocked_navigation_count: u64,
    pub os: &'static str,
    pub arch: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectResponse {
    pub probe: ServerProbeResult,
    pub saved_default: bool,
}

#[derive(Debug, Clone, Serialize)]
struct DiagnosticsReport {
    os: &'static str,
    arch: &'static str,
    configured_origin: String,
    active_origin: Option<String>,
    launcher_loaded_ms: Option<u64>,
    probe_finished_ms: Option<u64>,
    server_loaded_ms: Option<u64>,
    probe_ok: bool,
    probe_error: Option<String>,
    blocked_navigation_count: u64,
}

struct DesktopRuntimeState {
    started_at: Instant,
    config_path: PathBuf,
    configured_origin: ServerOrigin,
    source: TargetSource,
    saved_server_url: Option<ServerOrigin>,
    active_origin: Option<ServerOrigin>,
    launcher_url: Url,
    auto_connect: bool,
    manage_local_server: bool,
    supervisor_root: PathBuf,
    server_bin_override: Option<PathBuf>,
    allow_local: bool,
    managed_supervisor_starting: bool,
    managed_supervisor: Option<ManagedLocalServer>,
    last_error: Option<String>,
    blocked_navigation_count: u64,
    launcher_loaded_ms: Option<u64>,
    probe_finished_ms: Option<u64>,
    server_loaded_ms: Option<u64>,
    probe_ok: bool,
    diagnostics_file: Option<PathBuf>,
    smoke_exit_ms: Option<u64>,
    smoke_exit_scheduled: bool,
    quitting: bool,
}

impl DesktopRuntimeState {
    fn new(options: LaunchOptions) -> Self {
        let smoke_exit_ms = options.smoke_exit_ms.map(|ms| ms.min(MAX_SMOKE_EXIT_MS));
        Self {
            started_at: Instant::now(),
            config_path: options.target.config_path,
            configured_origin: options.target.origin,
            source: options.target.source,
            saved_server_url: options.target.saved_server_url,
            active_origin: None,
            launcher_url: default_launcher_url(),
            auto_connect: options.auto_connect,
            manage_local_server: options.target.manage_local_server,
            supervisor_root: options.target.supervisor_root,
            server_bin_override: options.server_bin,
            allow_local: options.allow_local,
            managed_supervisor_starting: false,
            managed_supervisor: None,
            last_error: None,
            blocked_navigation_count: 0,
            launcher_loaded_ms: None,
            probe_finished_ms: None,
            server_loaded_ms: None,
            probe_ok: false,
            diagnostics_file: options.diagnostics_file,
            smoke_exit_ms,
            smoke_exit_scheduled: false,
            quitting: false,
        }
    }

    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn snapshot(&self) -> DesktopStateSnapshot {
        DesktopStateSnapshot {
            server_url: self.configured_origin.as_str().to_owned(),
            source: self.source,
            config_path: self.config_path.display().to_string(),
            saved_server_url: self
                .saved_server_url
                .as_ref()
                .map(|origin| origin.as_str().to_owned()),
            active_origin: self
                .active_origin
                .as_ref()
                .map(|origin| origin.as_str().to_owned()),
            auto_connect: self.auto_connect,
            manage_local_server: self.manage_local_server,
            supervisor_root: self.supervisor_root.display().to_string(),
            managed_server: self
                .managed_supervisor
                .as_ref()
                .map(ManagedLocalServer::snapshot),
            last_error: self.last_error.clone(),
            blocked_navigation_count: self.blocked_navigation_count,
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
        }
    }

    fn diagnostics_report(&self) -> DiagnosticsReport {
        DiagnosticsReport {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            configured_origin: self.configured_origin.as_str().to_owned(),
            active_origin: self
                .active_origin
                .as_ref()
                .map(|origin| origin.as_str().to_owned()),
            launcher_loaded_ms: self.launcher_loaded_ms,
            probe_finished_ms: self.probe_finished_ms,
            server_loaded_ms: self.server_loaded_ms,
            probe_ok: self.probe_ok,
            probe_error: self.last_error.clone(),
            blocked_navigation_count: self.blocked_navigation_count,
        }
    }
}

struct ManagedDesktopState(Mutex<DesktopRuntimeState>);

impl ManagedDesktopState {
    fn lock(&self) -> std::sync::MutexGuard<'_, DesktopRuntimeState> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Runs the Tauri desktop application event loop.
///
/// # Errors
///
/// Returns an error if the Tauri application fails to initialize.
pub fn run(options: LaunchOptions) -> Result<(), String> {
    let runtime_state = ManagedDesktopState(Mutex::new(DesktopRuntimeState::new(options)));

    let app = tauri::Builder::default()
        .manage(runtime_state)
        .invoke_handler(tauri::generate_handler![
            get_desktop_state,
            connect_server,
            save_server_url,
            set_manage_local_server,
            restart_managed_server,
            record_launcher_probe_failure,
        ])
        .setup(|app| {
            let menu = build_menu(app.handle())?;
            app.set_menu(menu)?;
            create_main_window(app.handle())?;
            Ok(())
        })
        .on_menu_event(handle_menu_event)
        .build(tauri::generate_context!())
        .map_err(|error| format!("unable to initialize KeepPeek Desktop: {error}"))?;

    app.run(handle_run_event);
    Ok(())
}

#[tauri::command]
fn get_desktop_state(state: State<'_, ManagedDesktopState>) -> DesktopStateSnapshot {
    state.lock().snapshot()
}

#[tauri::command]
async fn connect_server(
    app: AppHandle,
    state: State<'_, ManagedDesktopState>,
    server_url: String,
    save_default: bool,
) -> Result<ConnectResponse, String> {
    let origin = ServerOrigin::parse_user_input(&server_url).map_err(|error| error.to_string())?;
    ensure_managed_local_server_if_opted_in(&state, &origin).await?;

    let probe_origin = origin.clone();
    let probe_result = tauri::async_runtime::spawn_blocking(move || {
        probe::probe_server(&probe_origin, ProbeOptions::default())
    })
    .await
    .map_err(|error| format!("Connection check did not complete: {error}."))?;

    let probe = match probe_result {
        Ok(result) => result,
        Err(error) => {
            let message = error.recovery_message(&origin);
            {
                let mut guard = state.lock();
                guard.configured_origin = origin;
                guard.active_origin = None;
                guard.last_error = Some(message.clone());
                guard.probe_ok = false;
                guard.probe_finished_ms = Some(guard.elapsed_ms());
                write_diagnostics_if_configured(&guard);
            }
            return Err(message);
        }
    };

    let config_path = state.lock().config_path.clone();
    if save_default {
        config_store::save_server_url(&config_path, &origin).map_err(|error| error.to_string())?;
    }

    {
        let mut guard = state.lock();
        guard.configured_origin = origin.clone();
        guard.active_origin = Some(origin.clone());
        guard.last_error = None;
        guard.probe_ok = true;
        guard.probe_finished_ms = Some(guard.elapsed_ms());
        if save_default {
            guard.saved_server_url = Some(origin.clone());
            guard.source = TargetSource::ConfigDesktop;
        }
        write_diagnostics_if_configured(&guard);
    }

    let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) else {
        let mut guard = state.lock();
        guard.active_origin = None;
        return Err("KeepPeek Desktop main window is unavailable.".to_owned());
    };
    let _ = window.set_title(&format!("KeepPeek Desktop — {origin}"));
    if let Err(error) = window.navigate(origin.app_entry_url()) {
        let message = format!("Unable to open {origin}: {error}.");
        let mut guard = state.lock();
        guard.active_origin = None;
        guard.last_error = Some(message.clone());
        write_diagnostics_if_configured(&guard);
        let _ = window.set_title("KeepPeek Desktop");
        return Err(message);
    }

    Ok(ConnectResponse {
        probe,
        saved_default: save_default,
    })
}

#[tauri::command]
fn save_server_url(
    state: State<'_, ManagedDesktopState>,
    server_url: String,
) -> Result<DesktopStateSnapshot, String> {
    let origin = ServerOrigin::parse_user_input(&server_url).map_err(|error| error.to_string())?;
    let config_path = state.lock().config_path.clone();
    config_store::save_server_url(&config_path, &origin).map_err(|error| error.to_string())?;

    let mut guard = state.lock();
    guard.configured_origin = origin.clone();
    guard.saved_server_url = Some(origin);
    guard.source = TargetSource::ConfigDesktop;
    Ok(guard.snapshot())
}

#[tauri::command]
async fn set_manage_local_server(
    state: State<'_, ManagedDesktopState>,
    enabled: bool,
) -> Result<DesktopStateSnapshot, String> {
    let config_path = state.lock().config_path.clone();
    config_store::save_manage_local_server(&config_path, enabled)
        .map_err(|error| error.to_string())?;
    let to_stop = {
        let mut guard = state.lock();
        guard.manage_local_server = enabled;
        if !enabled {
            guard.managed_supervisor.take()
        } else {
            None
        }
    };
    if let Some(mut managed) = to_stop {
        let _ = tauri::async_runtime::spawn_blocking(move || managed.stop()).await;
    }
    Ok(state.lock().snapshot())
}

#[tauri::command]
async fn restart_managed_server(
    state: State<'_, ManagedDesktopState>,
) -> Result<DesktopStateSnapshot, String> {
    let prior_restarts = {
        let guard = state.lock();
        let Some(managed) = guard.managed_supervisor.as_ref() else {
            return Err(
                "No desktop-managed local KeepPeek server is currently running.".to_owned(),
            );
        };
        let count = managed.snapshot().core.restart_count;
        if !managed.restart_core() {
            return Err("Unable to trigger managed local KeepPeek restart.".to_owned());
        }
        count
    };

    let deadline = Instant::now() + Duration::from_millis(DEFAULT_LOCAL_READY_TIMEOUT_MS);
    while Instant::now() < deadline {
        let done = {
            let guard = state.lock();
            if guard.quitting {
                return Err("KeepPeek Desktop is exiting.".to_owned());
            }
            guard.managed_supervisor.as_ref().map_or_else(
                || {
                    Some(Err(
                        "Managed local KeepPeek server stopped during restart.".to_owned()
                    ))
                },
                |managed| {
                    let core = managed.snapshot().core;
                    if core.restart_count > prior_restarts
                        && core.phase == keeppeek_supervisor::supervisor::ComponentPhase::Running
                    {
                        Some(Ok(guard.snapshot()))
                    } else if core.phase == keeppeek_supervisor::supervisor::ComponentPhase::Failed
                    {
                        Some(Err(core.last_error.unwrap_or_else(|| {
                            "Managed local KeepPeek server failed during restart.".to_owned()
                        })))
                    } else {
                        None
                    }
                },
            )
        };
        if let Some(result) = done {
            return result;
        }
        tauri::async_runtime::spawn_blocking(|| {
            std::thread::sleep(Duration::from_millis(100));
        })
        .await
        .map_err(|error| format!("Restart wait interrupted: {error}."))?;
    }
    Err("Timed out waiting for managed local KeepPeek server to restart.".to_owned())
}

async fn ensure_managed_local_server_if_opted_in(
    state: &State<'_, ManagedDesktopState>,
    origin: &ServerOrigin,
) -> Result<(), String> {
    let startup_params = {
        let mut guard = state.lock();
        if !guard.manage_local_server || !origin.is_loopback() || guard.managed_supervisor.is_some()
        {
            return Ok(());
        }
        if guard.managed_supervisor_starting {
            return Err(
                "Managed local KeepPeek server is already starting. Wait a moment and retry."
                    .to_owned(),
            );
        }
        guard.managed_supervisor_starting = true;
        (
            guard.supervisor_root.clone(),
            guard.config_path.clone(),
            guard.server_bin_override.clone(),
            guard.allow_local,
        )
    };

    // If an external local KeepPeek server is already reachable on this origin, reuse it.
    let pre_probe_origin = origin.clone();
    let already_running = tauri::async_runtime::spawn_blocking(move || {
        probe::probe_server(
            &pre_probe_origin,
            ProbeOptions {
                timeout_ms: 800,
                ..ProbeOptions::default()
            },
        )
        .is_ok()
    })
    .await
    .unwrap_or(false);
    if already_running {
        state.lock().managed_supervisor_starting = false;
        return Ok(());
    }

    let (supervisor_root, config_path, server_bin_override, allow_local) = startup_params;
    let spawn_result = tauri::async_runtime::spawn_blocking(move || {
        let desktop_exe = std::env::current_exe().ok();
        let config = managed_server::build_desktop_supervisor_config(
            &supervisor_root,
            &config_path,
            server_bin_override.as_deref(),
            desktop_exe.as_deref(),
            allow_local,
        );
        let mut server = ManagedLocalServer::start(config)
            .map_err(|error| format!("Unable to start managed local KeepPeek server: {error}."))?;
        if let Err(error) = server.wait_ready(DEFAULT_LOCAL_READY_TIMEOUT_MS) {
            server.stop();
            return Err(format!(
                "Managed local KeepPeek server did not become ready: {error}."
            ));
        }
        Ok::<ManagedLocalServer, String>(server)
    })
    .await
    .map_err(|error| format!("Managed local server startup task failed: {error}."));

    let managed = match spawn_result {
        Ok(Ok(server)) => server,
        Ok(Err(message)) | Err(message) => {
            state.lock().managed_supervisor_starting = false;
            return Err(message);
        }
    };

    let abort_server = {
        let mut guard = state.lock();
        guard.managed_supervisor_starting = false;
        if guard.quitting || !guard.manage_local_server {
            Some(managed)
        } else {
            guard.managed_supervisor = Some(managed);
            None
        }
    };
    if let Some(mut server) = abort_server {
        let _ = tauri::async_runtime::spawn_blocking(move || server.stop()).await;
        return Err("KeepPeek Desktop stopped managed local server startup.".to_owned());
    }
    Ok(())
}

#[tauri::command]
fn record_launcher_probe_failure(app: AppHandle, state: State<'_, ManagedDesktopState>) {
    let delay = {
        let mut guard = state.lock();
        write_diagnostics_if_configured(&guard);
        take_smoke_exit_delay(&mut guard)
    };
    if let Some(delay_ms) = delay {
        schedule_smoke_exit(app, delay_ms);
    }
}

fn create_main_window(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let nav_handle = app.clone();
    let load_handle = app.clone();
    let popup_handle = app.clone();

    WebviewWindowBuilder::new(app, MAIN_WINDOW_LABEL, WebviewUrl::App("index.html".into()))
        .title("KeepPeek Desktop")
        .inner_size(1280.0, 820.0)
        .min_inner_size(800.0, 600.0)
        .on_navigation(move |url| enforce_navigation(&nav_handle, url))
        .on_new_window(move |url, _features| {
            if navigation::allow_new_window(&url) {
                return NewWindowResponse::Allow;
            }
            let state = popup_handle.state::<ManagedDesktopState>();
            let mut guard = state.lock();
            guard.blocked_navigation_count = guard.blocked_navigation_count.saturating_add(1);
            warn!(%url, "blocked desktop popup window request");
            NewWindowResponse::Deny
        })
        .on_page_load(move |webview, payload| handle_page_load(&load_handle, &webview, &payload))
        .build()
}

fn enforce_navigation(app: &AppHandle, url: &Url) -> bool {
    let state = app.state::<ManagedDesktopState>();
    let mut guard = state.lock();
    let decision = navigation::decide_navigation(url, guard.active_origin.as_ref());
    if decision.is_allowed() {
        return true;
    }
    guard.blocked_navigation_count = guard.blocked_navigation_count.saturating_add(1);
    warn!(%url, ?decision, "blocked untrusted desktop webview navigation");
    write_diagnostics_if_configured(&guard);
    false
}

fn handle_page_load(
    app: &AppHandle,
    _webview: &WebviewWindow,
    payload: &tauri::webview::PageLoadPayload<'_>,
) {
    if payload.event() != PageLoadEvent::Finished {
        return;
    }
    let url = payload.url();
    let delay = {
        let state = app.state::<ManagedDesktopState>();
        let mut guard = state.lock();
        if navigation::is_launcher_url(url) {
            if let Ok(mut base) = Url::parse(&format!(
                "{}/index.html",
                url.origin().ascii_serialization()
            )) {
                base.set_query(None);
                guard.launcher_url = base;
            }
            if guard.launcher_loaded_ms.is_none() {
                guard.launcher_loaded_ms = Some(guard.elapsed_ms());
            }
            write_diagnostics_if_configured(&guard);
            None
        } else if navigation::decide_navigation(url, guard.active_origin.as_ref())
            == NavigationDecision::AllowConfiguredServer
        {
            if guard.server_loaded_ms.is_none() {
                guard.server_loaded_ms = Some(guard.elapsed_ms());
            }
            write_diagnostics_if_configured(&guard);
            take_smoke_exit_delay(&mut guard)
        } else {
            None
        }
    };
    if let Some(delay_ms) = delay {
        schedule_smoke_exit(app.clone(), delay_ms);
    }
}

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let switch_server = MenuItem::with_id(
        app,
        MENU_SWITCH_SERVER,
        "Switch Server...",
        true,
        Some("CmdOrCtrl+Shift+S"),
    )?;
    let reconnect = MenuItem::with_id(
        app,
        MENU_RECONNECT,
        "Reconnect",
        true,
        Some("CmdOrCtrl+Shift+R"),
    )?;
    let reload = MenuItem::with_id(app, MENU_RELOAD, "Reload", true, Some("CmdOrCtrl+R"))?;
    let restart_local = MenuItem::with_id(
        app,
        MENU_RESTART_LOCAL_SERVER,
        "Restart Local Server",
        true,
        Some("CmdOrCtrl+Alt+R"),
    )?;
    let quit = MenuItem::with_id(
        app,
        MENU_QUIT,
        "Quit KeepPeek Desktop",
        true,
        Some("CmdOrCtrl+Q"),
    )?;

    let connection_menu = Submenu::with_items(
        app,
        "Connection",
        true,
        &[
            &switch_server,
            &reconnect,
            &reload,
            &restart_local,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let edit_menu = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;

    Menu::with_items(app, &[&connection_menu, &edit_menu])
}

fn handle_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    match event.id().as_ref() {
        MENU_SWITCH_SERVER => navigate_to_launcher(app, true),
        MENU_RECONNECT => navigate_to_launcher(app, false),
        MENU_RELOAD => {
            if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
                let _ = window.reload();
            }
        }
        MENU_RESTART_LOCAL_SERVER => {
            let state = app.state::<ManagedDesktopState>();
            let restarted = state
                .lock()
                .managed_supervisor
                .as_ref()
                .is_some_and(ManagedLocalServer::restart_core);
            if restarted {
                info!("triggered restart of desktop-managed local KeepPeek server");
                navigate_to_launcher(app, false);
            }
        }
        MENU_QUIT => {
            {
                let state = app.state::<ManagedDesktopState>();
                state.lock().quitting = true;
            }
            app.exit(0);
        }
        _ => {}
    }
}

fn navigate_to_launcher(app: &AppHandle, manual_select: bool) {
    let launcher_url = {
        let state = app.state::<ManagedDesktopState>();
        let mut guard = state.lock();
        guard.active_origin = None;
        guard.auto_connect = !manual_select;
        let mut url = guard.launcher_url.clone();
        url.set_query(if manual_select {
            Some("manual=1")
        } else {
            None
        });
        url
    };
    if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
        let _ = window.set_title("KeepPeek Desktop");
        if let Err(error) = window.navigate(launcher_url) {
            warn!(%error, "unable to navigate desktop window back to launcher");
        }
    }
}

fn handle_run_event(app: &AppHandle, event: RunEvent) {
    match event {
        #[cfg(target_os = "macos")]
        RunEvent::Reopen {
            has_visible_windows: false,
            ..
        } => {
            if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
                let _ = window.show();
                let _ = window.set_focus();
                navigate_to_launcher(app, false);
            } else if let Err(error) = create_main_window(app) {
                warn!(%error, "unable to reopen KeepPeek Desktop window");
            }
        }
        #[cfg(target_os = "macos")]
        RunEvent::WindowEvent {
            label,
            event: WindowEvent::CloseRequested { api, .. },
            ..
        } if label == MAIN_WINDOW_LABEL => {
            let quitting = app.state::<ManagedDesktopState>().lock().quitting;
            if !quitting {
                api.prevent_close();
                // Navigating to the launcher triggers pagehide so WebRTC sessions close cleanly.
                navigate_to_launcher(app, true);
                if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
                    let _ = window.hide();
                }
            }
        }
        RunEvent::ExitRequested { .. } => {
            let managed_to_stop = {
                let state = app.state::<ManagedDesktopState>();
                let mut guard = state.lock();
                guard.quitting = true;
                write_diagnostics_if_configured(&guard);
                guard.managed_supervisor.take()
            };
            if let Some(mut managed) = managed_to_stop {
                info!("KeepPeek Desktop exiting; stopping desktop-managed local KeepPeek server");
                managed.stop();
            } else {
                info!("KeepPeek Desktop exiting; external KeepPeek servers remain unaffected");
            }
        }
        _ => {}
    }
}

fn default_launcher_url() -> Url {
    #[cfg(target_os = "windows")]
    let raw = "http://tauri.localhost/index.html";
    #[cfg(not(target_os = "windows"))]
    let raw = "tauri://localhost/index.html";
    Url::parse(raw).expect("static launcher URL must parse")
}

fn take_smoke_exit_delay(state: &mut DesktopRuntimeState) -> Option<u64> {
    if state.smoke_exit_scheduled {
        return None;
    }
    let delay = state.smoke_exit_ms?;
    state.smoke_exit_scheduled = true;
    Some(delay)
}

fn schedule_smoke_exit(app: AppHandle, delay_ms: u64) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(delay_ms));
        {
            let state = app.state::<ManagedDesktopState>();
            let mut guard = state.lock();
            guard.quitting = true;
            write_diagnostics_if_configured(&guard);
        }
        app.exit(0);
    });
}

fn write_diagnostics_if_configured(state: &DesktopRuntimeState) {
    let Some(path) = state.diagnostics_file.as_ref() else {
        return;
    };
    let report = state.diagnostics_report();
    let Ok(bytes) = serde_json::to_vec_pretty(&report) else {
        return;
    };
    if let Some(parent) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, bytes);
}
