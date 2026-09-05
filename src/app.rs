//! Runs the shared KeepPeek application lifecycle.

use std::{path::Path, sync::Arc};

use crate::{
    access::AccessManager,
    api::{CameraId, CameraLifecycle, CameraStatus},
    backup::{self, BackupManager},
    battery_wake::BatteryWakeService,
    camera_database::CameraDatabase,
    cameras,
    config::{self, Config},
    event_forwarder::Runtime as EventForwarderRuntime,
    keeppeek::KeepPeekLoop,
    logging::LoggingService,
    notifications::{HealthMonitor as NotificationHealthMonitor, Runtime as NotificationRuntime},
    operational_events::OperationalEventMonitor,
    runtime::{Router, RouterMessage, WorkerEvent},
    server::{
        ServerState, bind_server_listener, camera_health_snapshots,
        serve_with_state_on_listener_ready,
    },
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
    crate::server::migrate_template_store(config_path)?;
    crate::event_forwarder::remove_legacy_outbox(config_path)?;
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
    let mut layout_camera_ids = cameras.keys().map(ToString::to_string).collect::<Vec<_>>();
    layout_camera_ids.sort_unstable();
    crate::server::migrate_peek_layout_configuration(config_path, &layout_camera_ids)?;

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
    let operational_event_store = event_store.clone();
    let event_forwarder =
        EventForwarderRuntime::open(cfg.event_forwarder.mqtt.clone(), shutdown.clone())?;
    let event_forwarder_handle = event_forwarder.handle();
    let recording_demand = storage_engine.demand();
    let recording_health = storage_engine.health();
    let webrtc = WebRtc::with_recording_demand(recording_demand.clone());
    let health_registry = HealthRegistry::new();
    let server_state = ServerState::new(
        &cfg,
        &camera_configs,
        &cameras,
        &storage_config,
        recording_demand,
        webrtc.clone(),
    );
    let backup_manager = BackupManager::open_with_config_update(
        config_path.to_path_buf(),
        server_state.configuration_update_lock(),
    )?;
    let access_manager = AccessManager::open_with_config_update(
        config_path,
        cfg.access_key,
        server_state.configuration_update_lock(),
    )?;
    let server_state = server_state
        .with_access_manager(access_manager)
        .with_camera_config_path(config_path.to_path_buf())
        .with_camera_database(camera_database)
        .with_logging(logging)
        .with_restart_control(shutdown.clone(), restart.clone())
        .with_event_store(event_store.clone())
        .with_health_registry(health_registry.clone())
        .with_recording_catalog(recording_catalog.handle())
        .with_backup_manager(backup_manager)
        .with_recording_health(recording_health.clone())
        .with_battery_wake(battery_wake.as_ref().map(BatteryWakeService::handle));
    let notification_runtime = NotificationRuntime::open_with_config_update(
        config_path,
        server_state.configuration_update_lock(),
    )?;
    let notification_handle = notification_runtime.handle();
    let notification_health = NotificationHealthMonitor::start(
        recording_health,
        notification_handle.clone(),
        shutdown.clone(),
    )?;
    let server_state = server_state
        .with_notifications(notification_handle.clone())
        .with_event_forwarder(event_forwarder_handle.clone());

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
    keeppeek.set_notifications(notification_handle.clone());
    keeppeek.set_event_forwarder(event_forwarder_handle.clone());
    if let Some(battery_wake) = &battery_wake {
        keeppeek.set_battery_wake(battery_wake.handle());
    }

    let server_state = server_state.with_camera_runtime(keeppeek.control());

    let operational_state = server_state.clone();
    let operational_router = router_tx.clone();
    let operational_events = OperationalEventMonitor::start(
        cfg.operational_events,
        operational_event_store,
        notification_handle,
        event_forwarder_handle,
        shutdown.clone(),
        move || camera_health_snapshots(&operational_router, &operational_state),
    )?;

    router.wait_and_drain(Some(std::time::Duration::ZERO))?;
    let listener = bind_server_listener(&cfg.host, cfg.port)?;

    let server_state_for_metadata = server_state.clone();
    let server_shutdown = shutdown.clone();
    let (server_ready_tx, server_ready_rx) = std::sync::mpsc::sync_channel(1);
    let server_handle = std::thread::Builder::new()
        .name("http-server".to_owned())
        .spawn(move || {
            let result = serve_with_state_on_listener_ready(
                listener,
                server_shutdown.clone(),
                router_tx,
                server_state,
                server_ready_tx,
            )
            .map(|_| ());
            if result.is_err() {
                server_shutdown.cancel();
            }
            result
        })
        .expect("failed to spawn HTTP server");

    let mut startup_error = server_ready_rx
        .recv()
        .err()
        .map(|_| anyhow::anyhow!("HTTP server stopped before reporting readiness"));
    if startup_error.is_some() {
        shutdown.cancel();
    }

    let (keeppeek_handle, mut camera_start_rx) = if startup_error.is_none() {
        server_state_for_metadata.enrich_camera_metadata_in_background(
            camera_configs.values().flatten().cloned().collect(),
        );
        let (camera_start_tx, camera_start_rx) = std::sync::mpsc::sync_channel(1);
        let camera_shutdown = shutdown.clone();
        let keeppeek_handle = std::thread::Builder::new()
            .name("keeppeek".to_string())
            .spawn(move || {
                let result = keeppeek.add_cameras(&cameras);
                if let Err(error) = &result {
                    tracing::error!(%error, "camera workers could not start");
                    camera_shutdown.cancel();
                } else {
                    tracing::info!("camera workers launched");
                }
                if camera_start_tx.send(result).is_err() {
                    camera_shutdown.cancel();
                }
                keeppeek.run();
            })
            .expect("failed to spawn KeepPeek worker");
        (Some(keeppeek_handle), Some(camera_start_rx))
    } else {
        (None, None)
    };

    while !shutdown.is_cancelled() && !router.is_shutting_down() {
        if let Some(result) = camera_start_rx
            .as_ref()
            .map(std::sync::mpsc::Receiver::try_recv)
        {
            match result {
                Ok(Ok(())) => {
                    if let Err(error) = mark_pending_restore_healthy(config_path) {
                        startup_error = Some(error);
                        shutdown.cancel();
                    }
                    camera_start_rx = None;
                }
                Ok(Err(error)) => {
                    startup_error = Some(error);
                    camera_start_rx = None;
                    shutdown.cancel();
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    startup_error = Some(anyhow::anyhow!(
                        "camera worker startup ended before reporting status"
                    ));
                    camera_start_rx = None;
                    shutdown.cancel();
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        router.wait_and_drain(Some(std::time::Duration::from_millis(100)))?;
    }
    shutdown.cancel();
    webrtc.shutdown();

    match server_handle.join() {
        Ok(Ok(())) => {}
        Ok(Err(error)) if startup_error.is_some() => startup_error = Some(error),
        Ok(Err(error)) => tracing::warn!(%error, "server stopped with error"),
        Err(_) if startup_error.is_some() => {
            startup_error = Some(anyhow::anyhow!("HTTP server panicked during startup"));
        }
        Err(_) => tracing::warn!("HTTP server panicked"),
    }

    if let Some(keeppeek_handle) = keeppeek_handle
        && keeppeek_handle.join().is_err()
    {
        tracing::warn!("KeepPeek worker panicked");
    }
    if let Some(camera_start_rx) = camera_start_rx {
        match camera_start_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => startup_error = Some(error),
            Err(_) if startup_error.is_none() => {
                startup_error = Some(anyhow::anyhow!(
                    "camera worker startup ended before reporting status"
                ));
            }
            Err(_) => {}
        }
    }

    if let Some(battery_wake) = battery_wake {
        battery_wake.join();
    }

    operational_events.join();
    event_forwarder.join();
    notification_health.join();
    notification_runtime.shutdown();
    tracing::info!("flushing and finalizing all recordings...");
    storage_engine.shutdown();
    recording_catalog.shutdown();
    tracing::info!("all recordings saved");

    if let Some(error) = startup_error {
        return match rollback_pending_restore(config_path) {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(anyhow::anyhow!(
                "application startup failed: {error:#}; restore rollback also failed: {rollback_error:#}"
            )),
        };
    }
    Ok(restart.is_requested())
}

fn mark_pending_restore_healthy(config_path: &Path) -> anyhow::Result<()> {
    let now_unix_ms = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis(),
    )?;
    backup::mark_restore_healthy(config_path, now_unix_ms)?;
    Ok(())
}

fn rollback_pending_restore(config_path: &Path) -> anyhow::Result<()> {
    let now_unix_ms = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis(),
    )?;
    backup::recover_pending_restore(config_path, now_unix_ms)?;
    Ok(())
}
