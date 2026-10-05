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

use crate::storage::catalog::locations::legacy::roots::{
    Capture as LegacyRootCapture, Role as LegacyRootRole, State as LegacyRootState,
};

fn runtime_root_capture_fixture() -> anyhow::Result<(PathBuf, RecordingCatalog, LegacyPaths)> {
    let (base, pinned) = crate::storage::volumes::root::test_root()?;
    drop(pinned);
    let media = base.join("media");
    std::fs::create_dir_all(media.join(".exports"))?;
    std::fs::create_dir(media.join("images"))?;
    let paths = LegacyPaths {
        active_root: media.clone(),
        archive_root: media.clone(),
        export_root: media.join(".exports"),
        thumbnail_root: media.join("images"),
        catalog_path: base.join("catalog.db"),
        export_history_path: media.join(".exports/history.json"),
    };
    let catalog = RecordingCatalog::open(&paths.catalog_path)?;
    Ok((base, catalog, paths))
}

#[test]
fn captured_shared_legacy_roots_are_readonly_and_refuse_directory_replacement() -> anyhow::Result<()>
{
    let (base, catalog, paths) = runtime_root_capture_fixture()?;
    let handle = catalog.handle();
    assert!(captured_root(&handle, LegacyRootRole::Active)?.is_none());
    std::fs::write(paths.active_root.join("known-legacy.mp4"), b"original")?;
    capture_roots(&handle, &paths)?;
    let Reply::LegacyRoot(LegacyRootState::Bound(active)) =
        handle.volume_location(Request::LegacyRoot(LegacyRootRole::Active))?
    else {
        panic!("active root missing")
    };
    let Reply::LegacyRoot(LegacyRootState::Bound(archive)) =
        handle.volume_location(Request::LegacyRoot(LegacyRootRole::Archive))?
    else {
        panic!("archive root missing")
    };
    assert_eq!(active.id, archive.id);
    assert_eq!(active.root_identity, archive.root_identity);
    assert!(!active.writable && !archive.writable);
    let usage = handle.volume_location(Request::Usage)?;
    let Reply::Usage(rows) = &usage else {
        panic!("usage missing")
    };
    assert_eq!(rows.len(), 3);
    assert!(
        rows.iter()
            .all(|row| row.allocated_bytes == 0 && row.reserved_bytes == 0)
    );
    let identity = captured_root(&handle, LegacyRootRole::Active)?
        .unwrap()
        .identity()
        .clone();
    let retained = base.join("retained-media");
    std::fs::rename(&paths.active_root, &retained)?;
    std::fs::create_dir(&paths.active_root)?;
    std::fs::write(
        paths.active_root.join("known-legacy.mp4"),
        b"unrelated replacement",
    )?;
    assert!(captured_root(&handle, LegacyRootRole::Active).is_err());
    assert!(captured_root(&handle, LegacyRootRole::Archive).is_err());
    assert!(capture_roots(&handle, &paths).is_err());
    assert_eq!(handle.volume_location(Request::Usage)?, usage);
    let replacement = base.join("retained-replacement");
    std::fs::rename(&paths.active_root, &replacement)?;
    std::fs::rename(&retained, &paths.active_root)?;
    assert_eq!(
        *captured_root(&handle, LegacyRootRole::Active)?
            .unwrap()
            .identity(),
        identity
    );
    assert_eq!(
        std::fs::read(paths.active_root.join("known-legacy.mp4"))?,
        b"original"
    );
    assert_eq!(
        std::fs::read(replacement.join("known-legacy.mp4"))?,
        b"unrelated replacement"
    );
    assert_eq!(handle.volume_location(Request::Usage)?, usage);
    drop(handle);
    catalog.shutdown();
    Ok(())
}

#[test]
fn roots_missing_at_capture_remain_offline_until_explicit_recapture() -> anyhow::Result<()> {
    let (base, catalog, paths) = runtime_root_capture_fixture()?;
    let handle = catalog.handle();
    let retained = base.join("offline-media");
    std::fs::rename(&paths.active_root, &retained)?;
    capture_roots(&handle, &paths)?;
    assert!(!paths.active_root.exists());
    for role in [
        LegacyRootRole::Active,
        LegacyRootRole::Archive,
        LegacyRootRole::Export,
        LegacyRootRole::Thumbnail,
    ] {
        assert!(matches!(
            handle.volume_location(Request::LegacyRoot(role))?,
            Reply::LegacyRoot(LegacyRootState::Offline)
        ));
        assert!(captured_root(&handle, role).is_err());
    }
    assert_eq!(
        handle.volume_location(Request::Usage)?,
        Reply::Usage(vec![])
    );
    std::fs::rename(&retained, &paths.active_root)?;
    assert!(captured_root(&handle, LegacyRootRole::Active).is_err());
    assert!(captured_root(&handle, LegacyRootRole::Export).is_err());
    assert_eq!(
        handle.volume_location(Request::Usage)?,
        Reply::Usage(vec![])
    );
    capture_roots(&handle, &paths)?;
    for role in [
        LegacyRootRole::Active,
        LegacyRootRole::Archive,
        LegacyRootRole::Export,
        LegacyRootRole::Thumbnail,
    ] {
        assert!(captured_root(&handle, role)?.is_some());
    }
    let Reply::Usage(rows) = handle.volume_location(Request::Usage)? else {
        panic!("usage missing")
    };
    assert_eq!(rows.len(), 3);
    assert!(
        rows.iter()
            .all(|row| row.allocated_bytes == 0 && row.reserved_bytes == 0)
    );
    drop(handle);
    catalog.shutdown();
    Ok(())
}

#[test]
fn recapture_reuses_shared_root_binding_after_initial_active_probe_failed() -> anyhow::Result<()> {
    let (_, catalog, paths) = runtime_root_capture_fixture()?;
    let handle = catalog.handle();
    let identity = crate::storage::volumes::root::Root::open(&paths.archive_root)?
        .identity()
        .clone();
    let archive = crate::storage::catalog::locations::Binding {
        id: "legacy-archive".into(),
        generation: 1,
        root: paths.archive_root.clone(),
        filesystem: identity.filesystem,
        root_identity: identity.directory,
        writable: false,
        draining: false,
        limit_bytes: None,
        minimum_free_bytes: 0,
    };
    let initial = LegacyRootCapture {
        paths: paths.clone(),
        roots: vec![
            (LegacyRootRole::Active, None),
            (LegacyRootRole::Archive, Some(archive.clone())),
            (LegacyRootRole::Export, None),
            (LegacyRootRole::Thumbnail, None),
        ],
    };
    assert_eq!(
        handle.volume_location(Request::CaptureLegacyRoots(Box::new(initial)))?,
        Reply::Bound
    );
    assert!(captured_root(&handle, LegacyRootRole::Active).is_err());
    capture_roots(&handle, &paths)?;
    for role in [LegacyRootRole::Active, LegacyRootRole::Archive] {
        let Reply::LegacyRoot(LegacyRootState::Bound(binding)) =
            handle.volume_location(Request::LegacyRoot(role))?
        else {
            panic!("shared root missing")
        };
        assert_eq!(*binding, archive);
        assert!(captured_root(&handle, role)?.is_some());
    }
    let Reply::Usage(rows) = handle.volume_location(Request::Usage)? else {
        panic!("usage missing")
    };
    assert_eq!(rows.len(), 3);
    assert!(
        rows.iter()
            .all(|row| row.allocated_bytes == 0 && row.reserved_bytes == 0)
    );
    drop(handle);
    catalog.shutdown();
    Ok(())
}
#[test]
fn verified_legacy_recording_rejects_replaced_root_even_when_original_file_returns()
-> anyhow::Result<()> {
    let (base, catalog, reference) = fixture()?;
    let handle = catalog.handle();
    std::fs::create_dir_all(reference.path.parent().unwrap())?;
    let bytes = b"original legacy recording";
    std::fs::write(&reference.path, bytes)?;
    let verified = verify_recording(&handle, &reference)?;
    let Reply::LegacyPaths(Some(paths)) = handle.volume_location(Request::LegacyPaths)? else {
        panic!("captured paths missing")
    };
    capture_roots(&handle, &paths)?;
    let usage = handle.volume_location(Request::Usage)?;
    let retained_root = base.join("retained-media");
    let retained_file = retained_root.join("camera/recording.mp4");
    std::fs::rename(&paths.active_root, &retained_root)?;
    std::fs::create_dir_all(reference.path.parent().unwrap())?;
    std::fs::rename(&retained_file, &reference.path)?;
    {
        let replacement = crate::storage::volumes::root::Root::open(&paths.active_root)?;
        let mut file = replacement.inspect_legacy("camera/recording.mp4")?;
        let (actual_bytes, identity, digest) = file.inspect_evidence()?;
        let evidence = verified.evidence.as_ref().unwrap();
        assert_eq!(actual_bytes, evidence.bytes);
        assert_eq!(identity, evidence.file_identity);
        assert_eq!(digest, evidence.digest);
    }
    assert!(verify_recording(&handle, &verified).is_err());
    assert_eq!(std::fs::read(&reference.path)?, bytes);
    assert_eq!(handle.volume_location(Request::Usage)?, usage);
    assert_eq!(
        handle.volume_location(Request::LegacyInventory(Action::Lookup(
            reference.object.clone()
        )))?,
        Reply::LegacyReference(Some(Box::new(verified.clone())))
    );
    std::fs::rename(&reference.path, &retained_file)?;
    std::fs::rename(&paths.active_root, base.join("replaced-media"))?;
    std::fs::rename(&retained_root, &paths.active_root)?;
    assert_eq!(verify_recording(&handle, &verified)?, verified);
    assert_eq!(std::fs::read(&reference.path)?, bytes);
    assert_eq!(handle.volume_location(Request::Usage)?, usage);
    drop(handle);
    catalog.shutdown();
    Ok(())
}
#[test]
fn legacy_volume_lookup_uses_binding_id_without_promoting_its_offline_namesake_role()
-> anyhow::Result<()> {
    use crate::storage::catalog::locations::{
        Binding,
        legacy::roots::{Capture, Role, State},
    };
    let (root, catalog, reference) = fixture()?;
    let handle = catalog.handle();
    let Reply::LegacyPaths(Some(paths)) = handle.volume_location(Request::LegacyPaths)? else {
        anyhow::bail!("captured paths missing");
    };
    std::fs::create_dir_all(&paths.archive_root)?;
    let pinned = crate::storage::volumes::root::Root::open(&paths.archive_root)?;
    let binding = Binding {
        id: "legacy-active".into(),
        generation: 1,
        root: paths.archive_root.clone(),
        filesystem: pinned.identity().filesystem.clone(),
        root_identity: pinned.identity().directory.clone(),
        writable: false,
        draining: false,
        limit_bytes: None,
        minimum_free_bytes: 0,
    };
    handle.volume_location(Request::CaptureLegacyRoots(Box::new(Capture {
        paths: *paths,
        roots: vec![
            (Role::Active, None),
            (Role::Archive, Some(binding)),
            (Role::Export, None),
            (Role::Thumbnail, None),
        ],
    })))?;
    let revision = handle.volume_ledger_revision()?;
    let usage = handle.volume_location(Request::Usage)?;
    assert_eq!(
        handle.volume_location(Request::LegacyRoot(Role::Active))?,
        Reply::LegacyRoot(State::Offline)
    );
    assert!(captured_root(&handle, Role::Active).is_err());
    let resolved = volume_root(&handle, "legacy-active")?.expect("captured binding resolves");
    assert_eq!(resolved.identity(), pinned.identity());
    assert_eq!(resolved.path(), root.join("media"));
    assert_eq!(handle.volume_ledger_revision()?, revision);
    assert_eq!(handle.volume_location(Request::Usage)?, usage);
    assert_eq!(
        handle.volume_location(Request::LegacyRoot(Role::Active))?,
        Reply::LegacyRoot(State::Offline)
    );
    assert_eq!(
        handle.volume_location(Request::Lookup(reference.object))?,
        Reply::Location(None)
    );
    drop(resolved);
    drop(pinned);
    drop(handle);
    catalog.shutdown();
    Ok(())
}
