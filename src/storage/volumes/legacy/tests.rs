use super::*;
use crate::storage::catalog::{CatalogRecording, RecordingCatalog, locations::legacy::LegacyPaths};
use std::path::PathBuf;

fn fixture() -> anyhow::Result<(PathBuf, RecordingCatalog, Reference)> {
    let (root, _) = crate::storage::volumes::root::test_root()?;
    let catalog = RecordingCatalog::open_for_adoption(&root.join("catalog.db"))?;
    let media = root.join("media");
    let snapshot = LegacyPaths {
        active_root: media.clone(),
        archive_root: media.clone(),
        export_root: media.join(".exports"),
        thumbnail_root: root.join("images"),
        catalog_path: root.join("catalog.db"),
        export_history_path: media.join(".exports/history.json"),
    };
    catalog
        .handle()
        .volume_location(Request::RegisterLegacyPaths(Box::new(snapshot)))?;
    catalog.handle().upsert_recording(CatalogRecording {
        id: "legacy".into(),
        stream_id: "camera/main".into(),
        source_id: Some("camera".into()),
        logical_stream_id: Some("main".into()),
        started_at_ms: 1000,
        ended_at_ms: Some(2000),
        path: media
            .join("camera/recording.mp4")
            .to_string_lossy()
            .into_owned(),
        init_offset: 0,
        init_len: 8,
        finalized: true,
    })?;
    let Reply::LegacyReferences(mut references) =
        catalog
            .handle()
            .volume_location(Request::LegacyInventory(Action::Recordings {
                after: None,
                limit: 1,
            }))?
    else {
        anyhow::bail!("invalid inventory reply");
    };
    anyhow::ensure!(references.len() == 1, "legacy reference missing");
    Ok((root, catalog, references.remove(0)))
}

#[test]
fn offline_legacy_reference_can_be_verified_after_its_original_root_returns() -> anyhow::Result<()>
{
    let (root, catalog, reference) = fixture()?;
    assert!(verify_recording(&catalog.handle(), &reference).is_err());
    assert!(!root.join("media").exists());
    std::fs::create_dir_all(reference.path.parent().unwrap())?;
    let bytes = b"unchanged legacy recording";
    std::fs::write(&reference.path, bytes)?;
    let verified = verify_recording(&catalog.handle(), &reference)?;
    let evidence = verified.evidence.as_ref().unwrap();
    assert_eq!(evidence.bytes, bytes.len() as u64);
    assert_eq!(
        evidence.digest,
        <sha2::Sha256 as sha2::Digest>::digest(bytes).as_slice()
    );
    assert_eq!(verify_recording(&catalog.handle(), &reference)?, verified);
    let Reply::Usage(usage) = catalog.handle().volume_location(Request::Usage)? else {
        anyhow::bail!("invalid usage reply");
    };
    assert!(usage.is_empty());
    assert_eq!(std::fs::read(&reference.path)?, bytes);
    catalog.shutdown();
    Ok(())
}

#[test]
fn changed_legacy_file_cannot_silently_replace_verified_evidence() -> anyhow::Result<()> {
    let (_, catalog, reference) = fixture()?;
    std::fs::create_dir_all(reference.path.parent().unwrap())?;
    std::fs::write(&reference.path, b"original")?;
    let verified = verify_recording(&catalog.handle(), &reference)?;
    std::fs::write(&reference.path, b"modified")?;
    assert!(verify_recording(&catalog.handle(), &verified).is_err());
    assert_eq!(std::fs::read(&reference.path)?, b"modified");
    let actual = catalog
        .handle()
        .volume_location(Request::LegacyInventory(Action::Lookup(reference.object)))?;
    assert_eq!(actual, Reply::LegacyReference(Some(Box::new(verified))));
    catalog.shutdown();
    Ok(())
}

#[test]
fn physical_verification_matches_the_existing_legacy_catalog_identity_format() -> anyhow::Result<()>
{
    let (_, catalog, reference) = fixture()?;
    std::fs::create_dir_all(reference.path.parent().unwrap())?;
    std::fs::write(&reference.path, b"original")?;
    catalog
        .handle()
        .update_recording_path(&reference.object.id, &reference.path, true)?;
    let Reply::LegacyReference(Some(current)) = catalog
        .handle()
        .volume_location(Request::LegacyInventory(Action::Lookup(reference.object)))?
    else {
        anyhow::bail!("legacy reference missing");
    };
    assert!(
        verify_recording(&catalog.handle(), &current)?
            .evidence
            .is_some()
    );
    catalog.shutdown();
    Ok(())
}
