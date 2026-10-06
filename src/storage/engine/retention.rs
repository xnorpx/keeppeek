//! Runs bounded catalog reconciliation and expiry independently of the encoded-media writer.

use super::StorageConfig;
use crate::storage::catalog::{CatalogDeletionReason, RecordingCatalogHandle};
use crate::storage::long_term::LongTermStore;
use anyhow::{Context, Result};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

pub(super) struct Worker {
    shutdown: Arc<AtomicBool>,
    wake: mpsc::SyncSender<()>,
    thread: std::thread::JoinHandle<()>,
}

impl Worker {
    pub(super) fn start(
        config: StorageConfig,
        catalog: Option<RecordingCatalogHandle>,
    ) -> Result<Option<Self>> {
        let Some(catalog) = catalog else {
            anyhow::ensure!(
                config.retention.is_none(),
                "retention requires a recording catalog"
            );
            return Ok(None);
        };
        let accepted = catalog.request_retention_settings(config.retention.as_ref())?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let stop = shutdown.clone();
        let (wake, receiver) = mpsc::sync_channel(1);
        let dispatch = tracing::dispatcher::get_default(Clone::clone);
        let thread = std::thread::Builder::new()
            .name("recording-retention".into())
            .spawn(move || {
                tracing::dispatcher::with_default(&dispatch, || {
                    run(config, catalog, stop, receiver, accepted);
                });
            })
            .context("spawn retention worker")?;
        Ok(Some(Self {
            shutdown,
            wake,
            thread,
        }))
    }

    pub(super) fn shutdown(self) {
        self.shutdown.store(true, Ordering::Release);
        match self.wake.try_send(()) {
            Ok(())
            | Err(mpsc::TrySendError::Full(()))
            | Err(mpsc::TrySendError::Disconnected(())) => {}
        }
        if let Err(error) = self.thread.join() {
            tracing::error!(?error, "retention worker panicked");
        }
    }
}

fn run(
    config: StorageConfig,
    catalog: RecordingCatalogHandle,
    shutdown: Arc<AtomicBool>,
    wake: mpsc::Receiver<()>,
    mut accepted: bool,
) {
    let run_id = uuid::Uuid::new_v4();
    let _span = tracing::info_span!("storage.retention.runtime",%run_id).entered();
    tracing::info!(event="retention_started",%run_id,retention_enabled=config.retention.is_some(),request_accepted=accepted,"retention worker started");
    let mut initial_settled = false;
    let mut quarantined_seen = false;
    let store = LongTermStore::new(config.long_term_path.clone());
    // This loop lasts until shutdown. Each iteration evaluates at most eight rows and admits one expiry per owner.
    while !shutdown.load(Ordering::Acquire) {
        if !accepted {
            match catalog.request_retention_settings(config.retention.as_ref()) {
                Ok(value) => accepted = value,
                Err(error) => tracing::warn!(%run_id,%error,"retention settings request deferred"),
            }
        }
        if let Err(error) = recover_legacy(&catalog, &store) {
            tracing::warn!(%run_id,%error,"interrupted legacy expiry deferred");
        }
        let delay = match catalog.reconcile_retention_runtime(8) {
            Ok(progress) => {
                if !initial_settled {
                    quarantined_seen |= progress.quarantined > 0;
                    if accepted && !progress.activation_pending {
                        tracing::info!(event="retention_activation_ready",%run_id,quarantined_seen,"initial retention activation ready");
                        initial_settled = true;
                    }
                }
                if progress.quarantined > 0 {
                    tracing::warn!(
                        %run_id,
                        count = progress.quarantined,
                        "retention quarantined files remain protected from cleanup"
                    );
                }
                if accepted && config.retention.is_none() && !progress.pending {
                    break;
                }
                let mut expiry_work = false;
                if !progress.activation_pending {
                    match expire_batch(&config, &catalog, &store) {
                        Ok(work) => expiry_work = work,
                        Err(error) => tracing::warn!(%run_id,%error,"retention expiry deferred"),
                    }
                }
                if progress.pending || expiry_work {
                    Duration::from_millis(10)
                } else {
                    Duration::from_millis(250)
                }
            }
            Err(error) => {
                tracing::warn!(%run_id,%error,"retention reconciliation deferred");
                Duration::from_secs(1)
            }
        };
        match wake.recv_timeout(delay) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

pub(super) fn expire_legacy(
    catalog: &RecordingCatalogHandle,
    store: &LongTermStore,
    id: &str,
) -> Result<bool> {
    let Some(_owner) = catalog.try_legacy_cleanup_owner()? else {
        return Ok(false);
    };
    expire_legacy_owned(catalog, store, id)
}

fn recover_legacy(catalog: &RecordingCatalogHandle, store: &LongTermStore) -> Result<bool> {
    let Some(_owner) = catalog.try_legacy_cleanup_owner()? else {
        return Ok(false);
    };
    if let Some(candidate) = catalog.pending_cleanup_candidate()?
        && candidate.retention_expiry
    {
        return expire_legacy_owned(catalog, store, &candidate.recording_id);
    }
    Ok(false)
}

fn expire_legacy_owned(
    catalog: &RecordingCatalogHandle,
    store: &LongTermStore,
    id: &str,
) -> Result<bool> {
    let Some(candidate) = catalog.claim_expired_recording(store.root(), id)? else {
        return Ok(false);
    };
    let bytes = store.remove_catalog_recording(&candidate.path)?;
    catalog.complete_cleanup(
        &candidate.recording_id,
        CatalogDeletionReason::RetentionExpiry,
    )?;
    tracing::info!(
        recording_id = candidate.recording_id,
        bytes_removed = bytes,
        "recording retention expired whole file"
    );
    Ok(true)
}

fn expire_batch(
    config: &StorageConfig,
    catalog: &RecordingCatalogHandle,
    store: &LongTermStore,
) -> Result<bool> {
    use crate::storage::catalog::locations::{Kind, Object, Reply, Request};
    let records = catalog.expired_retention_candidates(8)?;
    let work = !records.is_empty();
    for id in records {
        if let Some(manager) = &config.volume_runtime
            && let Reply::Location(Some(location)) =
                catalog.volume_location(Request::Lookup(Object {
                    kind: Kind::Recording,
                    id: id.clone(),
                }))?
        {
            manager.queue_recording_expiry(
                &crate::storage::volumes::VolumeId::parse(&location.volume)?,
                &id,
            )?;
        } else {
            expire_legacy(catalog, store, &id)?;
        }
    }
    Ok(work)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::catalog::{CatalogRecording, RecordingCatalog};

    fn capture_activation(
        config: StorageConfig,
        handle: RecordingCatalogHandle,
    ) -> crate::logging::LogHub {
        use tracing_subscriber::prelude::*;
        // ponytail: A second live dispatcher avoids tracing's single-dispatcher shortcut in parallel tests.
        let _unobserved_dispatch = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::new());
        let hub = crate::logging::LogHub::default();
        let subscriber = tracing_subscriber::Registry::default()
            .with(crate::logging::LogCaptureLayer::new(hub.clone()))
            .with(tracing_subscriber::filter::LevelFilter::INFO);
        tracing::subscriber::with_default(subscriber, || {
            let worker = Worker::start(config, Some(handle)).unwrap().unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(8);
            while std::time::Instant::now() < deadline {
                if hub.snapshot(None, 100).entries.iter().any(|entry| {
                    entry.fields.get("event")
                        == Some(&serde_json::json!("retention_activation_ready"))
                }) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            worker.shutdown();
        });
        hub
    }

    #[test]
    fn retention_telemetry_correlates_activation_and_quarantine_without_configuration_values() {
        let root =
            std::env::temp_dir().join(format!("retention-telemetry-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let catalog = RecordingCatalog::open(&root.join("catalog.db")).unwrap();
        let handle = catalog.handle();
        let path = root.join("invalid.mp4");
        std::fs::write(&path, b"media").unwrap();
        handle
            .upsert_recording(CatalogRecording {
                id: "invalid".into(),
                stream_id: "front/main".into(),
                source_id: Some("front".into()),
                logical_stream_id: Some("main".into()),
                started_at_ms: 1000,
                ended_at_ms: Some(1000),
                path: path.to_string_lossy().into_owned(),
                init_offset: 0,
                init_len: 0,
                finalized: true,
            })
            .unwrap();
        let settings = toml::from_str("[default]\ncontinuous_days=1.0").unwrap();
        handle.request_retention_settings(Some(&settings)).unwrap();
        let config = StorageConfig {
            retention: Some(settings),
            long_term_path: root.clone(),
            ..StorageConfig::default()
        };
        let hub = capture_activation(config, handle);
        let entries = hub.snapshot(None, 100).entries;
        let started = entries
            .iter()
            .find(|entry| {
                entry.fields.get("event") == Some(&serde_json::json!("retention_started"))
            })
            .unwrap_or_else(|| panic!("retention startup event missing: {entries:?}"));
        let ready = entries
            .iter()
            .find(|entry| {
                entry.fields.get("event") == Some(&serde_json::json!("retention_activation_ready"))
            })
            .unwrap_or_else(|| panic!("retention readiness event missing: {entries:?}"));
        assert_eq!(started.fields["run_id"], ready.fields["run_id"]);
        uuid::Uuid::parse_str(started.fields["run_id"].as_str().unwrap()).unwrap();
        assert_eq!(ready.fields["quarantined_seen"], true);
        assert!(
            entries.iter().any(
                |entry| entry.fields.get("recording_id") == Some(&serde_json::json!("invalid"))
            )
        );
        assert!(
            !serde_json::to_string(&entries)
                .unwrap()
                .contains(root.to_str().unwrap())
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"media");
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }

    fn insert_recording(
        handle: &RecordingCatalogHandle,
        root: &std::path::Path,
        id: &str,
        media: &[u8],
    ) -> std::path::PathBuf {
        let path = root.join(format!("{id}.mp4"));
        std::fs::write(&path, media).unwrap();
        handle
            .upsert_recording(CatalogRecording {
                id: id.into(),
                stream_id: "front/main".into(),
                source_id: Some("front".into()),
                logical_stream_id: Some("main".into()),
                started_at_ms: 1000,
                ended_at_ms: Some(2000),
                path: path.to_string_lossy().into_owned(),
                init_offset: 0,
                init_len: 0,
                finalized: true,
            })
            .unwrap();
        path
    }

    #[test]
    fn admitted_expiry_recovers_before_pending_policy_activation() {
        let root =
            std::env::temp_dir().join(format!("retention-recovery-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let catalog = RecordingCatalog::open(&root.join("catalog.db")).unwrap();
        let handle = catalog.handle();
        let path = insert_recording(&handle, &root, "expired", b"media");
        let settings = toml::from_str("[default]\ncontinuous_days=0.0").unwrap();
        handle.request_retention_settings(Some(&settings)).unwrap();
        for _ in 0..16 {
            if !handle.reconcile_retention_runtime(4).unwrap().pending {
                break;
            }
        }
        assert!(
            handle
                .claim_expired_recording(&root, "expired")
                .unwrap()
                .is_some()
        );
        let longer = toml::from_str("[default]\ncontinuous_days=30.0").unwrap();
        handle.request_retention_settings(Some(&longer)).unwrap();
        assert!(
            handle
                .reconcile_retention_runtime(1)
                .unwrap()
                .activation_pending
        );
        assert!(recover_legacy(&handle, &LongTermStore::new(root.clone())).unwrap());
        assert!(!path.exists());
        let mut pending = true;
        for _ in 0..16 {
            pending = handle
                .reconcile_retention_runtime(4)
                .unwrap()
                .activation_pending;
            if !pending {
                break;
            }
        }
        assert!(!pending, "interrupted expiry blocked activation");
        assert!(handle.pending_cleanup_candidate().unwrap().is_none());
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn configured_runtime_expires_whole_media_without_capacity_pressure() {
        let root = std::env::temp_dir().join(format!("retention-media-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let config_path = root.join("config.toml");
        std::fs::write(
            &config_path,
            "[storage.retention.default]\ncontinuous_days=0.0\n",
        )
        .unwrap();
        let loaded = crate::config::load_config(&config_path).unwrap();
        let mut config = StorageConfig::from_toml(&loaded.storage);
        config.medium_term_path = root.clone();
        config.long_term_path = root.clone();
        config.recording_catalog_path = root.join("catalog.db");
        config.long_term_max_bytes = 0;
        config.minimum_free_bytes = 0;
        config.maximum_used_percent = None;
        let catalog = RecordingCatalog::open(&config.recording_catalog_path).unwrap();
        let handle = catalog.handle();
        let media = include_bytes!("../../../crates/test-camera/testdata/cc-4k-640x360-h264.mp4");
        for id in ["expired", "protected"] {
            insert_recording(&handle, &root, id, media);
        }
        handle.set_recording_protected("protected", true).unwrap();
        std::fs::write(root.join("unindexed.mp4"), media).unwrap();
        let engine = super::super::StorageEngine::start_with_catalog(config, handle);
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        while root.join("expired.mp4").exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        engine.shutdown();
        assert!(
            !root.join("expired.mp4").exists(),
            "expiry required capacity pressure"
        );
        assert_eq!(std::fs::read(root.join("protected.mp4")).unwrap(), media);
        assert_eq!(std::fs::read(root.join("unindexed.mp4")).unwrap(), media);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }
}
