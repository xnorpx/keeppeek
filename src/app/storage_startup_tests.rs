use super::open_recording_catalog;
use crate::storage::{
    RecordingCatalog, StorageConfig,
    catalog::{
        CatalogRecording,
        locations::{Reply, Request, legacy::LegacyPaths},
    },
    volumes::{Volume, VolumeConfiguration, VolumeId, VolumeRole, VolumeState},
};
use std::path::PathBuf;

fn fixture(state: VolumeState) -> anyhow::Result<(PathBuf, StorageConfig)> {
    let root = std::env::temp_dir().join(format!("keeppeek-app-storage-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("active"))?;
    std::fs::create_dir_all(root.join("metadata"))?;
    let config = StorageConfig {
        medium_term_path: root.join("active"),
        long_term_path: root.join("offline-archive"),
        event_thumbnail_path: root.join("thumbnails"),
        recording_catalog_path: root.join("metadata/catalog.db"),
        named_volumes: Some(VolumeConfiguration {
            volumes: vec![Volume {
                id: VolumeId::parse("named")?,
                root: root.join("named"),
                roles: vec![VolumeRole::Active],
                state,
                priority: 0,
                capacity_bytes: None,
                minimum_free_bytes: 0,
                warning_free_bytes: 0,
                critical_free_bytes: 0,
                sources: vec![],
                groups: vec![],
            }],
            placement: vec![],
        }),
        ..StorageConfig::default()
    };
    seed_missing_recording(&config)?;
    Ok((root, config))
}

fn seed_missing_recording(config: &StorageConfig) -> anyhow::Result<()> {
    let media = config.medium_term_path.join("missing.mp4");
    // ponytail: the normal finalization API captures size before the fixture removes the file.
    std::fs::write(&media, [42_u8; 64])?;
    let catalog = RecordingCatalog::open(&config.recording_catalog_path)?;
    catalog.handle().upsert_recording(CatalogRecording {
        id: "legacy".into(),
        stream_id: "camera/main".into(),
        source_id: Some("camera".into()),
        logical_stream_id: Some("main".into()),
        started_at_ms: 1000,
        ended_at_ms: Some(2000),
        path: media.to_string_lossy().into_owned(),
        init_offset: 0,
        init_len: 8,
        finalized: true,
    })?;
    catalog
        .handle()
        .update_recording_path("legacy", &media, true)?;
    assert_eq!(catalog.handle().stats()?.recording_bytes, 64);
    catalog.shutdown();
    std::fs::remove_file(media)?;
    Ok(())
}

fn expected_paths(config: &StorageConfig) -> LegacyPaths {
    LegacyPaths {
        active_root: config.medium_term_path.clone(),
        archive_root: config.long_term_path.clone(),
        thumbnail_root: config.event_thumbnail_path.clone(),
        catalog_path: config.recording_catalog_path.clone(),
        export_root: config.long_term_path.join(".exports"),
        export_history_path: config.long_term_path.join(".exports/history.json"),
    }
}

#[test]
fn enabled_named_startup_captures_legacy_paths_before_missing_file_reconciliation()
-> anyhow::Result<()> {
    let (root, config) = fixture(VolumeState::Enabled)?;
    let mut catalog = open_recording_catalog(&config)?;
    catalog.wait_for_maintenance();
    assert_eq!(
        catalog.handle().volume_location(Request::LegacyPaths)?,
        Reply::LegacyPaths(Some(Box::new(expected_paths(&config))))
    );
    let stats = catalog.handle().stats()?;
    assert_eq!(stats.recording_files, 1);
    assert_eq!(stats.finalized_files, 1);
    assert_eq!(stats.recording_bytes, 64);
    assert!(!config.medium_term_path.join("missing.mp4").exists());
    assert!(!config.long_term_path.exists());
    assert!(!root.join("named").exists());
    catalog.shutdown();
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn disabled_only_named_startup_retains_uncaptured_legacy_missing_file_behavior()
-> anyhow::Result<()> {
    let (root, config) = fixture(VolumeState::Disabled)?;
    let mut catalog = open_recording_catalog(&config)?;
    catalog.wait_for_maintenance();
    assert_eq!(
        catalog.handle().volume_location(Request::LegacyPaths)?,
        Reply::LegacyPaths(None)
    );
    assert_eq!(catalog.handle().stats()?.recording_files, 0);
    assert_eq!(catalog.handle().stats()?.recording_bytes, 0);
    assert!(!config.long_term_path.exists());
    assert!(!root.join("named").exists());
    catalog.shutdown();
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn captured_paths_cannot_change_after_all_named_volumes_are_disabled() -> anyhow::Result<()> {
    let (root, mut config) = fixture(VolumeState::Enabled)?;
    let catalog = open_recording_catalog(&config)?;
    let expected = Reply::LegacyPaths(Some(Box::new(expected_paths(&config))));
    assert_eq!(
        catalog.handle().volume_location(Request::LegacyPaths)?,
        expected
    );
    catalog.shutdown();
    config.named_volumes.as_mut().unwrap().volumes[0].state = VolumeState::Disabled;
    for role in ["active", "archive", "thumbnail"] {
        let mut changed = config.clone();
        let replacement = root.join(format!("changed-{role}"));
        match role {
            "active" => changed.medium_term_path = replacement.clone(),
            "archive" => changed.long_term_path = replacement.clone(),
            "thumbnail" => changed.event_thumbnail_path = replacement.clone(),
            _ => unreachable!(),
        }
        assert!(
            open_recording_catalog(&changed).is_err(),
            "changed {role} root was accepted"
        );
        assert!(!replacement.exists());
        let mut catalog = open_recording_catalog(&config)?;
        catalog.wait_for_maintenance();
        assert_eq!(
            catalog.handle().volume_location(Request::LegacyPaths)?,
            expected
        );
        assert_eq!(catalog.handle().stats()?.recording_files, 1);
        assert_eq!(catalog.handle().stats()?.recording_bytes, 64);
        catalog.shutdown();
    }
    std::fs::remove_dir_all(root)?;
    Ok(())
}
