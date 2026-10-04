use crate::{
    config::{StorageMigration, StorageMigrationPaths},
    storage::{
        RecordingCatalog,
        catalog::{
            CatalogRecording,
            locations::{Reply, Request, legacy::LegacyPaths},
        },
    },
};
use std::path::{Path, PathBuf};

fn fixture() -> anyhow::Result<(PathBuf, LegacyPaths)> {
    let root = std::env::temp_dir().join(format!(
        "keeppeek-captured-migration-{}",
        uuid::Uuid::new_v4()
    ));
    let media = root.join("current-media");
    std::fs::create_dir_all(media.join("thumbnails"))?;
    std::fs::write(media.join("recording.mp4"), [42_u8; 64])?;
    std::fs::write(media.join("thumbnails/event.jpg"), b"retained thumbnail")?;
    let snapshot = LegacyPaths {
        active_root: media.clone(),
        archive_root: media.clone(),
        thumbnail_root: media.join("thumbnails"),
        catalog_path: root.join("current-metadata/catalog.db"),
        export_root: media.join(".exports"),
        export_history_path: media.join(".exports/history.json"),
    };
    let catalog = RecordingCatalog::open(&snapshot.catalog_path)?;
    catalog.handle().upsert_recording(CatalogRecording {
        id: "legacy".into(),
        stream_id: "camera/main".into(),
        source_id: Some("camera".into()),
        logical_stream_id: Some("main".into()),
        started_at_ms: 1000,
        ended_at_ms: Some(2000),
        path: media.join("recording.mp4").to_string_lossy().into_owned(),
        init_offset: 0,
        init_len: 8,
        finalized: true,
    })?;
    catalog
        .handle()
        .update_recording_path("legacy", &media.join("recording.mp4"), true)?;
    catalog
        .handle()
        .volume_location(Request::RegisterLegacyPaths(Box::new(snapshot.clone())))?;
    assert_eq!(catalog.handle().stats()?.recording_bytes, 64);
    catalog.shutdown();
    Ok((root, snapshot))
}

fn migration(
    root: &Path,
    snapshot: &LegacyPaths,
    move_catalog: bool,
) -> anyhow::Result<StorageMigration> {
    let next = root.join("next-media");
    let next_catalog = if move_catalog {
        root.join("next-metadata/catalog.db")
    } else {
        snapshot.catalog_path.clone()
    };
    StorageMigration::between_with_metadata(
        StorageMigrationPaths::new(
            &snapshot.active_root,
            &snapshot.archive_root,
            &snapshot.catalog_path,
            &snapshot.thumbnail_root,
        ),
        StorageMigrationPaths::new(&next, &next, &next_catalog, &next.join("thumbnails")),
    )?
    .ok_or_else(|| anyhow::anyhow!("migration fixture produced no route"))
}

fn assert_source_authority_preserved(snapshot: &LegacyPaths) -> anyhow::Result<()> {
    // ponytail: ordinary opening already verifies the physical identity and rejects source fences.
    let mut catalog = RecordingCatalog::open(&snapshot.catalog_path)?;
    catalog.wait_for_maintenance();
    assert_eq!(
        catalog.handle().volume_location(Request::LegacyPaths)?,
        Reply::LegacyPaths(Some(Box::new(snapshot.clone())))
    );
    let stats = catalog.handle().stats()?;
    assert_eq!(stats.recording_files, 1);
    assert_eq!(stats.finalized_files, 1);
    assert_eq!(stats.recording_bytes, 64);
    catalog.shutdown();
    Ok(())
}

fn refused_migration_preserves_every_source(
    move_catalog: bool,
    offline: bool,
) -> anyhow::Result<()> {
    let (root, snapshot) = fixture()?;
    let migration = migration(&root, &snapshot, move_catalog)?;
    let retained_media = if offline {
        let retained = root.join("disconnected-media");
        std::fs::rename(&snapshot.active_root, &retained)?;
        retained
    } else {
        snapshot.active_root.clone()
    };
    for _ in 0..2 {
        assert!(migration.apply().is_err());
        assert!(!root.join("next-media").exists());
        assert!(!root.join("next-metadata").exists());
        assert_eq!(
            std::fs::read(retained_media.join("recording.mp4"))?,
            [42_u8; 64]
        );
        assert_eq!(
            std::fs::read(retained_media.join("thumbnails/event.jpg"))?,
            b"retained thumbnail"
        );
        if offline {
            assert!(!snapshot.active_root.exists());
        }
        assert_source_authority_preserved(&snapshot)?;
    }
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn captured_snapshot_refuses_media_migration_with_unchanged_external_catalog() -> anyhow::Result<()>
{
    for offline in [false, true] {
        refused_migration_preserves_every_source(false, offline)?;
    }
    Ok(())
}

#[test]
fn captured_snapshot_refuses_catalog_and_media_migration_before_destination_creation()
-> anyhow::Result<()> {
    for offline in [false, true] {
        refused_migration_preserves_every_source(true, offline)?;
    }
    Ok(())
}

#[test]
fn captured_snapshot_refuses_migration_without_catalog_metadata() -> anyhow::Result<()> {
    let (root, snapshot) = fixture()?;
    let mut migration = migration(&root, &snapshot, false)?;
    migration.recording_catalog_after_move = None;
    assert!(migration.apply().is_err());
    assert!(!root.join("next-media").exists());
    assert_eq!(
        std::fs::read(snapshot.active_root.join("recording.mp4"))?,
        [42_u8; 64]
    );
    assert_source_authority_preserved(&snapshot)?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}
