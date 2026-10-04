use super::{Kind, Manager, Object, Reply, Request, Reservation, VolumeRole, tests::fixture};
use crate::storage::catalog::RecordingCatalog;
use std::{fs, io::Write};

fn reserve(manager: &Manager, bytes: u64) -> anyhow::Result<(Object, Reservation)> {
    let object = Object {
        kind: Kind::Export,
        id: uuid::Uuid::new_v4().to_string(),
    };
    let reservation = manager
        .reserve(VolumeRole::Export, "camera", &[], object.clone(), bytes)?
        .expect("fixture export policy matches");
    Ok((object, reservation))
}

fn allocated_bytes(catalog: &RecordingCatalog) -> anyhow::Result<u64> {
    let Reply::Usage(usage) = catalog.handle().volume_location(Request::Usage)? else {
        anyhow::bail!("volume usage reply is missing");
    };
    Ok(usage.iter().map(|volume| volume.allocated_bytes).sum())
}

#[test]
fn published_export_retirement_removes_only_its_owned_file() -> anyhow::Result<()> {
    let (root, catalog, manager) = fixture(1024)?;
    let (object, reservation) = reserve(&manager, 64)?;
    let path = reservation.path().to_path_buf();
    let unrelated = root
        .join("primary")
        .join(format!("{}.mp4", uuid::Uuid::new_v4()));
    fs::write(&unrelated, b"unrelated export")?;
    let mut writer = reservation.open()?;
    writer.write_all(b"owned export")?;
    let evidence = writer.evidence()?;
    writer.publish(evidence)?;
    drop(writer);
    assert_eq!(allocated_bytes(&catalog)?, 12);
    catalog
        .handle()
        .volume_location(Request::RetireExport(object.id.clone()))?;
    assert!(manager.finish_export_retirement(&object.id)?);
    assert!(!path.exists());
    assert_eq!(allocated_bytes(&catalog)?, 0);
    assert_eq!(fs::read(&unrelated)?, b"unrelated export");
    assert_eq!(
        catalog
            .handle()
            .volume_location(Request::Lookup(object.clone()))?,
        Reply::Location(None)
    );
    assert!(manager.finish_export_retirement(&object.id)?);
    drop(manager);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn pending_export_retirement_handles_absent_empty_and_partial_owned_files() -> anyhow::Result<()> {
    for written in [None, Some(0), Some(13)] {
        let (root, catalog, manager) = fixture(1024)?;
        let (object, reservation) = reserve(&manager, 64)?;
        let path = reservation.path().to_path_buf();
        if let Some(bytes) = written {
            let mut writer = reservation.open()?;
            writer.write_all(&vec![7; bytes])?;
            drop(writer);
        } else {
            drop(reservation);
        }
        assert_eq!(allocated_bytes(&catalog)?, 64);
        catalog
            .handle()
            .volume_location(Request::RetireExport(object.id.clone()))?;
        assert!(manager.finish_export_retirement(&object.id)?);
        assert!(!path.exists());
        assert_eq!(allocated_bytes(&catalog)?, 0);
        assert!(manager.finish_export_retirement(&object.id)?);
        drop(manager);
        catalog.shutdown();
        fs::remove_dir_all(root)?;
    }
    Ok(())
}

#[test]
fn retirement_preserves_unknown_file_at_an_unmaterialized_reservation() -> anyhow::Result<()> {
    let (root, catalog, manager) = fixture(1024)?;
    let (object, reservation) = reserve(&manager, 64)?;
    let path = reservation.path().to_path_buf();
    drop(reservation);
    fs::write(&path, b"not created by the reserved writer")?;
    catalog
        .handle()
        .volume_location(Request::RetireExport(object.id.clone()))?;
    assert!(!matches!(
        manager.finish_export_retirement(&object.id),
        Ok(true)
    ));
    assert_eq!(fs::read(&path)?, b"not created by the reserved writer");
    assert_eq!(allocated_bytes(&catalog)?, 64);
    assert!(
        manager
            .reserve(VolumeRole::Export, "camera", &[], object, 64)
            .is_err()
    );
    drop(manager);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn export_retirement_waits_until_the_artifact_worker_releases_its_lease() -> anyhow::Result<()> {
    let (root, catalog, manager) = fixture(1024)?;
    let (object, reservation) = reserve(&manager, 64)?;
    let path = reservation.path().to_path_buf();
    let lease = catalog.handle().claim_volume_move(&object.id)?;
    let mut writer = reservation.open()?;
    writer.write_all(b"unfinished export")?;
    catalog
        .handle()
        .volume_location(Request::RetireExport(object.id.clone()))?;
    assert!(!matches!(
        manager.finish_export_retirement(&object.id),
        Ok(true)
    ));
    assert_eq!(fs::read(&path)?, b"unfinished export");
    assert_eq!(allocated_bytes(&catalog)?, 64);
    drop(writer);
    drop(lease);
    assert!(manager.finish_export_retirement(&object.id)?);
    assert!(!path.exists());
    assert_eq!(allocated_bytes(&catalog)?, 0);
    drop(manager);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn export_retirement_preserves_existing_readers_and_refuses_new_readers() -> anyhow::Result<()> {
    let (root, catalog, manager) = fixture(1024)?;
    let (object, reservation) = reserve(&manager, 64)?;
    let path = reservation.path().to_path_buf();
    let mut writer = reservation.open()?;
    writer.write_all(b"owned export")?;
    let evidence = writer.evidence()?;
    writer.publish(evidence)?;
    drop(writer);
    let (_, reader) = catalog
        .handle()
        .leased_export(&object.id)?
        .expect("published export");
    catalog
        .handle()
        .volume_location(Request::RetireExport(object.id.clone()))?;
    assert!(catalog.handle().leased_export(&object.id).is_err());
    manager.finish_export_retirement(&object.id)?;
    assert!(path.exists());
    assert_eq!(allocated_bytes(&catalog)?, 12);
    drop(reader);
    manager.finish_export_retirement(&object.id)?;
    assert!(!path.exists());
    assert_eq!(allocated_bytes(&catalog)?, 0);
    drop(manager);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn export_retirement_defers_on_read_only_volumes() -> anyhow::Result<()> {
    let (root, catalog, manager) = fixture(1024)?;
    let (object, reservation) = reserve(&manager, 64)?;
    let path = reservation.path().to_path_buf();
    let mut writer = reservation.open()?;
    writer.write_all(b"partial")?;
    drop(writer);
    catalog
        .handle()
        .volume_location(Request::RetireExport(object.id.clone()))?;
    let mut configuration = manager.inner.configuration.clone();
    configuration.volumes[0].state = super::VolumeState::ReadOnly;
    drop(manager);
    let unavailable = Manager::new(configuration.clone(), catalog.handle())?;
    assert!(unavailable.finish_export_retirement(&object.id).is_err());
    assert!(path.exists());
    assert_eq!(allocated_bytes(&catalog)?, 64);
    drop(unavailable);
    configuration.volumes[0].state = super::VolumeState::Enabled;
    let available = Manager::new(configuration, catalog.handle())?;
    available.finish_export_retirement(&object.id)?;
    assert!(!path.exists());
    assert_eq!(allocated_bytes(&catalog)?, 0);
    drop(available);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn export_cleanup_accepts_existing_simple_ids_and_ignores_other_worker_ids() -> anyhow::Result<()> {
    let (root, catalog, manager) = fixture(1024)?;
    assert!(!manager.finish_export_retirement("legacy-move-id")?);
    let object = Object {
        kind: Kind::Export,
        id: uuid::Uuid::new_v4().simple().to_string(),
    };
    let reservation = manager
        .reserve(VolumeRole::Export, "camera", &[], object.clone(), 64)?
        .unwrap();
    let path = reservation.path().to_path_buf();
    let mut writer = reservation.open()?;
    writer.write_all(b"export")?;
    let evidence = writer.evidence()?;
    writer.publish(evidence)?;
    drop(writer);
    catalog
        .handle()
        .volume_location(Request::RetireExport(object.id.clone()))?;
    manager.finish_export_retirement(&object.id)?;
    assert!(!path.exists());
    assert_eq!(allocated_bytes(&catalog)?, 0);
    drop(manager);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn export_retirement_recovers_a_removed_file_before_catalog_completion() -> anyhow::Result<()> {
    use crate::storage::catalog::locations::{export_cleanup::Action, moves::Cancellation};
    let (root, catalog, manager) = fixture(1024)?;
    let (object, reservation) = reserve(&manager, 64)?;
    let path = reservation.path().to_path_buf();
    let mut writer = reservation.open()?;
    writer.write_all(b"export")?;
    let evidence = writer.evidence()?;
    writer.publish(evidence.clone())?;
    drop(writer);
    catalog
        .handle()
        .volume_location(Request::RetireExport(object.id.clone()))?;
    let key = path.file_name().unwrap().to_str().unwrap().to_owned();
    let captured = Cancellation::File {
        relative_key: key.clone(),
        bytes: evidence.bytes,
        file_identity: evidence.file_identity.clone(),
        digest: evidence.digest,
    };
    catalog
        .handle()
        .volume_location(Request::ExportCleanup(Action::Verify(
            object.id.clone(),
            captured,
        )))?;
    manager.inner.writable_root(0)?.retire_owned(
        &key,
        &evidence.file_identity,
        evidence.bytes,
        evidence.digest,
        &evidence.operation,
    )?;
    assert!(!path.exists());
    assert_eq!(allocated_bytes(&catalog)?, 6);
    let configuration = manager.inner.configuration.clone();
    drop(manager);
    let recovered = Manager::new(configuration, catalog.handle())?;
    recovered.finish_export_retirement(&object.id)?;
    assert_eq!(allocated_bytes(&catalog)?, 0);
    recovered.finish_export_retirement(&object.id)?;
    drop(recovered);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn cancellation_during_file_creation_keeps_the_identity_for_cleanup() -> anyhow::Result<()> {
    let (root, catalog, manager) = fixture(1024)?;
    let (object, reservation) = reserve(&manager, 64)?;
    let path = reservation.path().to_path_buf();
    let lease = catalog.handle().claim_volume_move(&object.id)?;
    catalog
        .handle()
        .volume_location(Request::RetireExport(object.id.clone()))?;
    let writer = reservation.open()?;
    drop(writer);
    drop(lease);
    manager.finish_export_retirement(&object.id)?;
    assert!(!path.exists());
    assert_eq!(allocated_bytes(&catalog)?, 0);
    drop(manager);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}
