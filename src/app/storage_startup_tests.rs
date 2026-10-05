use super::open_recording_catalog;
use crate::storage::volumes::VolumeId;
use crate::storage::{RecordingCatalog, StorageConfig};
use std::path::PathBuf;

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
    RecordingCatalog::open(&storage.recording_catalog_path)?.shutdown();
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
