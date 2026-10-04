use super::{
    LegacyPaths,
    inventory::{Action, Evidence},
};
use crate::storage::{
    catalog::{
        CatalogRecording, RecordingCatalog,
        locations::{Reply, Request},
    },
    engine::StorageConfig,
    volumes::{Volume, VolumeConfiguration, VolumeId, VolumeRole, VolumeState},
};
use std::path::{Path, PathBuf};

fn paths(root: &Path) -> LegacyPaths {
    LegacyPaths {
        active_root: root.join("media"),
        archive_root: root.join("media"),
        export_root: root.join("exports"),
        thumbnail_root: root.join("images"),
        catalog_path: root.join("catalog.db"),
        export_history_path: root.join("exports/history.json"),
    }
}

fn seed_recording(catalog: &RecordingCatalog, root: &Path) -> anyhow::Result<PathBuf> {
    let media = root.join("media/recording.mp4");
    std::fs::create_dir_all(media.parent().unwrap())?;
    // ponytail: use the existing MP4 fixture instead of adding another media generator.
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("crates/test-camera/testdata/cc-4k-640x360-h264.mp4"),
        &media,
    )?;
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
    assert_eq!(
        catalog.handle().stats()?.recording_bytes,
        std::fs::metadata(&media)?.len()
    );
    assert!(catalog.handle().stats()?.recording_bytes > 0);
    Ok(media)
}

#[test]
fn registered_legacy_inventory_survives_normal_startup_with_unavailable_media() -> anyhow::Result<()>
{
    for (verified, offline_root) in [(false, false), (true, false), (true, true)] {
        let root = crate::storage::catalog::tests::test_dir("legacy-inventory-startup");
        let snapshot = paths(&root);
        let catalog = RecordingCatalog::open(&snapshot.catalog_path)?;
        let media = seed_recording(&catalog, &root)?;
        let before = catalog.handle().stats()?;
        catalog
            .handle()
            .volume_location(Request::RegisterLegacyPaths(Box::new(snapshot.clone())))?;
        let reference = register_reference(&catalog, &media, verified)?;
        assert_eq!(reference.evidence.is_some(), verified);
        catalog.shutdown();
        if offline_root {
            std::fs::rename(&snapshot.archive_root, root.join("offline-media"))?;
        } else {
            std::fs::remove_file(&media)?;
        }
        let mut catalog = RecordingCatalog::open(&snapshot.catalog_path)?;
        catalog.wait_for_maintenance();
        let after = catalog.handle().stats()?;
        assert_eq!(after.recording_files, before.recording_files);
        assert_eq!(after.finalized_files, before.finalized_files);
        assert_eq!(after.recording_bytes, before.recording_bytes);
        assert_eq!(
            catalog.handle().volume_location(Request::LegacyPaths)?,
            Reply::LegacyPaths(Some(Box::new(snapshot)))
        );
        assert_eq!(
            catalog
                .handle()
                .volume_location(Request::LegacyInventory(Action::Lookup(
                    reference.object.clone()
                ),))?,
            Reply::LegacyReference(Some(Box::new(reference)))
        );
        assert!(!media.exists());
        catalog.shutdown();
        std::fs::remove_dir_all(root)?;
    }
    Ok(())
}

fn register_reference(
    catalog: &RecordingCatalog,
    media: &Path,
    verified: bool,
) -> anyhow::Result<super::inventory::Reference> {
    let Reply::LegacyReferences(mut page) =
        catalog
            .handle()
            .volume_location(Request::LegacyInventory(Action::Recordings {
                after: None,
                limit: 1,
            }))?
    else {
        anyhow::bail!("legacy inventory page missing")
    };
    assert_eq!(page.len(), 1);
    let reference = page.remove(0);
    let reference = if verified {
        let Reply::LegacyReference(Some(reference)) =
            catalog
                .handle()
                .volume_location(Request::LegacyInventory(Action::Verify(
                    Box::new(reference),
                    evidence(media)?,
                )))?
        else {
            anyhow::bail!("verified legacy reference missing")
        };
        *reference
    } else {
        reference
    };
    Ok(reference)
}

fn evidence(path: &Path) -> anyhow::Result<Evidence> {
    let metadata = std::fs::metadata(path)?;
    let identity = crate::storage::catalog::recording_file_identity(path, &metadata)
        .ok_or_else(|| anyhow::anyhow!("fixture file identity unavailable"))?;
    Ok(Evidence {
        file_identity: identity.clone(),
        catalog_identity: identity,
        bytes: metadata.len(),
        digest: <sha2::Sha256 as sha2::Digest>::digest(std::fs::read(path)?).into(),
    })
}

fn disabled_draft(root: &Path) -> anyhow::Result<VolumeConfiguration> {
    Ok(VolumeConfiguration {
        volumes: vec![Volume {
            id: VolumeId::parse("draft")?,
            root: root.join("unavailable-draft"),
            roles: vec![VolumeRole::Active],
            state: VolumeState::Disabled,
            priority: 0,
            capacity_bytes: None,
            minimum_free_bytes: 0,
            warning_free_bytes: 0,
            critical_free_bytes: 0,
            sources: vec![],
            groups: vec![],
        }],
        placement: vec![],
    })
}

#[test]
fn disabled_draft_does_not_capture_or_adopt_legacy_paths_on_restart() -> anyhow::Result<()> {
    let root = crate::storage::catalog::tests::test_dir("legacy-disabled-draft-startup");
    let catalog_path = root.join("catalog.db");
    let catalog = RecordingCatalog::open(&catalog_path)?;
    let media = seed_recording(&catalog, &root)?;
    let mut config = StorageConfig {
        named_volumes: Some(disabled_draft(&root)?),
        ..StorageConfig::default()
    };
    config.initialize_named_volumes(catalog.handle())?;
    assert!(config.volume_runtime.is_none());
    assert_eq!(
        catalog.handle().volume_location(Request::LegacyPaths)?,
        Reply::LegacyPaths(None)
    );
    catalog.shutdown();
    std::fs::remove_file(&media)?;
    let mut catalog = RecordingCatalog::open(&catalog_path)?;
    config.initialize_named_volumes(catalog.handle())?;
    catalog.wait_for_maintenance();
    assert!(config.volume_runtime.is_none());
    assert_eq!(
        catalog.handle().volume_location(Request::LegacyPaths)?,
        Reply::LegacyPaths(None)
    );
    assert_eq!(
        catalog.handle().volume_location(Request::Usage)?,
        Reply::Usage(vec![])
    );
    assert_eq!(catalog.handle().stats()?.recording_files, 0);
    assert_eq!(catalog.handle().stats()?.recording_bytes, 0);
    assert!(!root.join("unavailable-draft").exists());
    catalog.shutdown();
    std::fs::remove_dir_all(root)?;
    Ok(())
}
