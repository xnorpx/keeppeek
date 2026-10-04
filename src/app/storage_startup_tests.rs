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
fn managed_startup_fixture() -> anyhow::Result<(PathBuf, StorageConfig)> {
    let root =
        std::env::temp_dir().join(format!("keeppeek-managed-startup-{}", uuid::Uuid::new_v4()));
    let metadata = root.join("metadata");
    std::fs::create_dir_all(&metadata)?;
    let handoff = uuid::Uuid::new_v4();
    let mut storage = StorageConfig {
        medium_term_path: root.join("active"),
        long_term_path: root.join("offline-archive"),
        event_thumbnail_path: root.join("thumbnails"),
        recording_catalog_path: metadata.join(format!("catalog-{handoff}.db")),
        metadata_history_path: Some(metadata.join(format!("exports-{handoff}.json"))),
        named_volumes: Some(toml::from_str(&format!(
            "[[volumes]]\nid='metadata-owner'\nroot='{}'\nroles=['metadata']\nstate='enabled'\n",
            metadata.display()
        ))?),
        ..StorageConfig::default()
    };
    let snapshot = LegacyPaths::effective(&storage)?;
    RecordingCatalog::open_with_legacy_paths(&storage.recording_catalog_path, &snapshot)?
        .shutdown();
    let mut lease =
        crate::storage::catalog::authority::Lease::acquire(&storage.recording_catalog_path)?;
    let connection = lease.connect()?;
    let authority = lease.verify(&connection)?;
    drop(connection);
    drop(lease);
    let identity = crate::storage::volumes::root::Root::open(&metadata)?
        .identity()
        .clone();
    storage.metadata = Some(crate::config::MetadataBinding {
        volume_id: VolumeId::parse("metadata-owner")?,
        catalog_file: format!("catalog-{handoff}.db"),
        history_file: format!("exports-{handoff}.json"),
        catalog_id: authority.catalog_id,
        generation: authority.generation,
        filesystem: identity.filesystem,
        root_identity: identity.directory,
    });
    std::fs::write(
        storage.metadata_history_path.as_ref().unwrap(),
        b"{\"version\":1,\"jobs\":[]}\n",
    )?;
    Ok((root, storage))
}

#[test]
fn managed_startup_requires_valid_existing_history_before_exposing_catalog_workers()
-> anyhow::Result<()> {
    let (_root, storage) = managed_startup_fixture()?;
    let history = storage.metadata_history_path.as_ref().unwrap();
    let catalog_before = std::fs::read(&storage.recording_catalog_path)?;
    let authority = storage.metadata.as_ref().unwrap().authority();
    for invalid in [
        "{malformed-json",
        r#"{"version":2,"jobs":[]}"#,
        r#"{"version":1,"jobs":[{"requester_id":"owner","artifact_id":"attempt","request":"%%","job":"","created_at_ms":1,"updated_at_ms":1}]}"#,
        r#"{"version":1,"jobs":[{"requester_id":"owner","artifact_id":"attempt","request":"AA","job":"AA","created_at_ms":1,"updated_at_ms":1}]}"#,
    ] {
        std::fs::write(history, invalid)?;
        assert!(
            open_recording_catalog(&storage).is_err(),
            "accepted history: {invalid}"
        );
        assert_eq!(std::fs::read(history)?, invalid.as_bytes());
        assert_eq!(
            std::fs::read(&storage.recording_catalog_path)?,
            catalog_before
        );
        let mut lease =
            crate::storage::catalog::authority::Lease::acquire(&storage.recording_catalog_path)?;
        let connection = lease.connect()?;
        assert_eq!(lease.verify(&connection)?, authority);
        drop(connection);
    }
    std::fs::remove_file(history)?;
    assert!(open_recording_catalog(&storage).is_err());
    assert!(!history.exists());
    assert_eq!(
        std::fs::read(&storage.recording_catalog_path)?,
        catalog_before
    );
    let mut lease =
        crate::storage::catalog::authority::Lease::acquire(&storage.recording_catalog_path)?;
    let connection = lease.connect()?;
    assert_eq!(lease.verify(&connection)?, authority);
    drop(connection);
    Ok(())
}

#[test]
fn managed_startup_accepts_valid_empty_history_and_preserves_catalog_authority()
-> anyhow::Result<()> {
    let (_root, storage) = managed_startup_fixture()?;
    let history = storage.metadata_history_path.as_ref().unwrap();
    let before = std::fs::read(history)?;
    let mut catalog = open_recording_catalog(&storage)?;
    catalog.wait_for_maintenance();
    assert_eq!(catalog.handle().stats()?.recording_files, 0);
    assert_eq!(
        catalog.handle().volume_location(Request::LegacyPaths)?,
        Reply::LegacyPaths(Some(Box::new(LegacyPaths::effective(&storage)?)))
    );
    catalog.shutdown();
    assert_eq!(std::fs::read(history)?, before);
    let mut lease =
        crate::storage::catalog::authority::Lease::acquire(&storage.recording_catalog_path)?;
    let connection = lease.connect()?;
    assert_eq!(
        lease.verify(&connection)?,
        storage.metadata.as_ref().unwrap().authority()
    );
    drop(connection);
    Ok(())
}
