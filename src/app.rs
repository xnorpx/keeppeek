//! Runs the shared KeepPeek application lifecycle.

use std::{path::Path, sync::Arc};

use crate::{
    api::{CameraId, CameraLifecycle, CameraStatus},
    battery_wake::BatteryWakeService,
    camera_database::CameraDatabase,
    cameras,
    config::{self, Config},
    keeppeek::KeepPeekLoop,
    logging::LoggingService,
    notifications::{HealthMonitor as NotificationHealthMonitor, Runtime as NotificationRuntime},
    runtime::{Router, RouterMessage, WorkerEvent},
    server::{ServerState, run_server},
    shutdown::{Restart, Shutdown},
    stats::HealthRegistry,
    storage::{EventStore, RecordingCatalog, StorageConfig, StorageEngine},
    webrtc::WebRtc,
};

#[cfg(windows)]
use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};

#[cfg(windows)]
struct WindowsTimerResolution(u32);

#[cfg(windows)]
impl WindowsTimerResolution {
    fn request(period_ms: u32) -> Option<Self> {
        // SAFETY: timeBeginPeriod accepts any u32 period and has no pointer or lifetime requirements.
        let result = unsafe { timeBeginPeriod(period_ms) };
        if result == 0 {
            Some(Self(period_ms))
        } else {
            tracing::warn!(%period_ms, %result, "unable to set Windows timer resolution");
            None
        }
    }
}

#[cfg(windows)]
impl Drop for WindowsTimerResolution {
    fn drop(&mut self) {
        // SAFETY: this exactly balances the successful timeBeginPeriod call made by request.
        unsafe {
            timeEndPeriod(self.0);
        }
    }
}

/// Runs KeepPeek until shutdown and reports whether configuration requested a restart.
pub fn run(
    cfg: Config,
    config_path: &Path,
    logging: LoggingService,
    shutdown: Shutdown,
    restart: Restart,
) -> anyhow::Result<bool> {
    #[cfg(windows)]
    let _timer_resolution = WindowsTimerResolution::request(1);

    let camera_database = Arc::new(CameraDatabase::load_embedded()?);
    let metadata = camera_database.metadata();
    tracing::info!(
        version = %metadata.version,
        tag = %metadata.tag,
        generated_at = %metadata.generated_at,
        camera_count = metadata.camera_count,
        "loaded CCTV camera database"
    );

    let camera_configs = config::load_cameras(config_path)?;
    tracing::info!(
        "loaded {} camera(s) from {}",
        camera_configs.values().map(|v| v.len()).sum::<usize>(),
        config_path.display(),
    );

    let cameras = cameras::configured_cameras(&camera_configs);
    tracing::info!("initialized {} camera(s) from configuration", cameras.len());

    for cam in cameras.values() {
        tracing::info!(
            name = cam.config.name.as_deref().unwrap_or("?"),
            ip = %cam.config.ip,
            backend = ?cam.config.backend,
            transport = ?cam.config.transport,
            main_endpoint = cam.config.main_rtsp_url.is_some(),
            sub_endpoint = cam.config.sub_rtsp_url.is_some(),
            "configured camera",
        );
    }

    let battery_wake = if cfg.battery_wake.enabled {
        let camera_uids = cameras.values().filter_map(|camera| {
            camera
                .config
                .uid
                .as_ref()
                .or(camera.device.p2p_uid.as_ref())
                .cloned()
        });
        let service =
            BatteryWakeService::start(cfg.battery_wake.clone(), camera_uids, shutdown.clone())?;
        tracing::info!(
            middleman_port = cfg.battery_wake.middleman_port,
            register_port = cfg.battery_wake.register_port,
            "battery camera wake service started",
        );
        Some(service)
    } else {
        None
    };

    let storage_config = StorageConfig::from_toml(&cfg.storage);
    let recording_catalog = RecordingCatalog::open(&storage_config.recording_catalog_path)?;
    let catalog_handle = recording_catalog.handle();
    for camera in cameras.values() {
        let source_id = camera.config.ip.to_string();
        let recording_label = camera
            .config
            .name
            .clone()
            .unwrap_or_else(|| source_id.clone());
        for stream_id in ["main", "sub"] {
            catalog_handle.backfill_recording_identity(
                &format!("{recording_label}/{stream_id}"),
                &source_id,
                stream_id,
            )?;
        }
    }
    let storage_engine =
        StorageEngine::start_with_catalog(storage_config.clone(), catalog_handle.clone());
    let event_store = EventStore::new(
        catalog_handle,
        &storage_config.event_thumbnail_path,
        storage_config.event_thumbnail_max_bytes,
    )?;
    let notification_path = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("notifications.db");
    let notification_runtime = NotificationRuntime::open(&notification_path)?;
    let notification_handle = notification_runtime.handle();
    let recording_demand = storage_engine.demand();
    let recording_health = storage_engine.health();
    let notification_health = NotificationHealthMonitor::start(
        recording_health.clone(),
        notification_handle.clone(),
        shutdown.clone(),
    )?;
    let webrtc = WebRtc::with_recording_demand(recording_demand.clone());
    let health_registry = HealthRegistry::new();
    let server_state = ServerState::new(
        &cfg,
        &camera_configs,
        &cameras,
        &storage_config,
        recording_demand,
        webrtc.clone(),
    )
    .with_camera_config_path(config_path.to_path_buf())
    .with_camera_database(camera_database)
    .with_logging(logging)
    .with_restart_control(shutdown.clone(), restart.clone())
    .with_event_store(event_store.clone())
    .with_health_registry(health_registry.clone())
    .with_recording_catalog(recording_catalog.handle())
    .with_recording_health(recording_health)
    .with_battery_wake(battery_wake.as_ref().map(BatteryWakeService::handle));
    let server_state = server_state.with_notifications(notification_handle.clone());

    let (mut router, router_tx) = Router::new()?;
    for camera in cameras.values() {
        let id = camera
            .config
            .name
            .clone()
            .unwrap_or_else(|| camera.config.ip.to_string());
        router_tx
            .send(RouterMessage::WorkerEvent(WorkerEvent::StatusChanged(
                CameraStatus {
                    id: CameraId::new(id),
                    lifecycle: CameraLifecycle::Starting,
                    expected_streams: Vec::new(),
                    connected_streams: Vec::new(),
                    last_error: None,
                },
            )))
            .map_err(|error| anyhow::anyhow!("unable to register camera with router: {error:?}"))?;
    }

    let mut keeppeek = KeepPeekLoop::new(shutdown.clone(), Some(storage_engine.handle()));
    keeppeek.set_live(webrtc.live());
    keeppeek.set_event_store(event_store);
    keeppeek.set_health_registry(health_registry);
    keeppeek.set_status_sender(router_tx.clone());
    keeppeek.set_notifications(notification_handle);
    if let Some(battery_wake) = &battery_wake {
        keeppeek.set_battery_wake(battery_wake.handle());
    }

    keeppeek.add_cameras(&cameras)?;
    let server_state = server_state.with_camera_runtime(keeppeek.control());
    server_state
        .enrich_camera_metadata_in_background(camera_configs.values().flatten().cloned().collect());

    let keeppeek_handle = std::thread::Builder::new()
        .name("keeppeek".to_string())
        .spawn(move || keeppeek.run())
        .expect("failed to spawn KeepPeek worker");

    router.wait_and_drain(Some(std::time::Duration::ZERO))?;
    tracing::info!("camera workers launched, starting HTTP server");

    let server_shutdown = shutdown.clone();
    let server_handle = std::thread::Builder::new()
        .name("http-server".to_owned())
        .spawn(move || {
            let result = run_server(server_state, server_shutdown.clone(), router_tx);
            if result.is_err() {
                server_shutdown.cancel();
            }
            result
        })
        .expect("failed to spawn HTTP server");

    while !shutdown.is_cancelled() && !router.is_shutting_down() {
        router.wait_and_drain(Some(std::time::Duration::from_millis(100)))?;
    }
    shutdown.cancel();
    webrtc.shutdown();

    match server_handle.join() {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!(%error, "server stopped with error"),
        Err(_) => tracing::warn!("HTTP server panicked"),
    }

    if keeppeek_handle.join().is_err() {
        tracing::warn!("KeepPeek worker panicked");
    }

    if let Some(battery_wake) = battery_wake {
        battery_wake.join();
    }

    notification_health.join();
    notification_runtime.shutdown();
    tracing::info!("flushing and finalizing all recordings...");
    storage_engine.shutdown();
    recording_catalog.shutdown();
    tracing::info!("all recordings saved");

    Ok(restart.is_requested())
}
