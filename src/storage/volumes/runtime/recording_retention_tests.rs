use super::*;
use crate::storage::catalog::locations::recordings::{Action, Job, Reason};
use crate::storage::{CatalogFragment, CatalogRecording, RecordingCatalog};

fn recording(
    manager: &Manager,
    catalog: &RecordingCatalog,
    started: i64,
    finalized: bool,
) -> anyhow::Result<(Object, PathBuf)> {
    let object = Object {
        kind: Kind::Recording,
        id: uuid::Uuid::new_v4().to_string(),
    };
    let reservation = manager
        .reserve(VolumeRole::Active, "camera", &[], object.clone(), 8)?
        .unwrap();
    let path = reservation.path().to_path_buf();
    let mut file = reservation.open()?;
    file.write_all(b"initdata")?;
    catalog.handle().upsert_recording(CatalogRecording {
        id: object.id.clone(),
        stream_id: "camera/main".into(),
        source_id: Some("camera".into()),
        logical_stream_id: Some("main".into()),
        started_at_ms: started,
        ended_at_ms: Some(started + 100),
        path: path.to_string_lossy().into_owned(),
        init_offset: 0,
        init_len: 4,
        finalized: false,
    })?;
    catalog.handle().insert_fragment_with_keyframe(
        CatalogFragment {
            recording_id: object.id.clone(),
            sequence: 1,
            start_ms: started,
            duration_ms: 100,
            byte_offset: 4,
            byte_len: 4,
            random_access: true,
        },
        crate::storage::CatalogKeyframe {
            recording_id: object.id.clone(),
            fragment_sequence: 1,
            byte_offset: 4,
            byte_len: 4,
        },
    )?;
    if finalized {
        let evidence = file.evidence()?;
        file.finalize(evidence)?;
    }
    Ok((object, path))
}

fn begin(catalog: &RecordingCatalog, volume: &str) -> anyhow::Result<Box<Job>> {
    let Reply::RecordingRetirement(Some(job)) =
        catalog
            .handle()
            .volume_location(Request::RecordingRetention(Action::Begin {
                volume: volume.into(),
                reason: Reason::Capacity,
            }))?
    else {
        anyhow::bail!("eligible recording retirement missing");
    };
    Ok(job)
}

fn usage(catalog: &RecordingCatalog) -> anyhow::Result<u64> {
    let Reply::Usage(usage) = catalog.handle().volume_location(Request::Usage)? else {
        anyhow::bail!("usage reply missing");
    };
    Ok(usage.iter().map(|volume| volume.allocated_bytes).sum())
}

fn deletion_job(
    handle: &crate::storage::RecordingCatalogHandle,
    object: &Object,
) -> anyhow::Result<crate::storage::catalog::maintenance::jobs::Job> {
    use crate::storage::catalog::maintenance::{Scope, jobs};
    use anyhow::Context;
    let scope = Scope::Recording {
        source_id: "camera".into(),
        stream_id: "main".into(),
        recording_id: object.id.clone(),
    };
    let snapshot = handle.recording_maintenance_snapshot(scope.clone())?;
    assert!(snapshot.recordings[0].file_identity.is_some());
    let mut job = handle
        .recording_deletion_intent(
            "admin",
            jobs::Action::Prepare(jobs::Intent {
                scope,
                reason: jobs::Reason::Operator,
                expected_revision: snapshot.revision,
            }),
        )
        .context("prepare named deletion")?;
    handle
        .recording_deletion_intent(
            "admin",
            jobs::Action::Confirm {
                id: job.id.clone(),
                nonce: job.confirmation.take().unwrap(),
                expected_revision: job.revision,
            },
        )
        .context("confirm named deletion")?;
    Ok(job)
}

#[test]
fn operator_deletion_of_named_recording_releases_ownership() -> anyhow::Result<()> {
    use anyhow::Context;
    let (root, catalog, manager) = tests::fixture(1024)?;
    let (object, path) = recording(&manager, &catalog, 1_000, true)?;
    let handle = catalog.handle();
    let job = deletion_job(&handle, &object)?;
    let archive = crate::storage::long_term::inspection::Archive::open(path.parent().unwrap())?;
    assert!(
        handle
            .execute_recording_deletion("admin", &job.id, &archive)
            .is_err()
    );
    let report = handle
        .execute_named_recording_deletion_authorized("admin", &job.id, &manager, |_| Ok(()))
        .context("execute named deletion")?;
    assert_eq!(report.deleted, 1);
    assert!(!path.exists());
    assert_eq!(usage(&catalog)?, 0);
    drop(archive);
    drop(manager);
    catalog.shutdown();
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn operator_deletion_waits_for_named_readers() -> anyhow::Result<()> {
    let (_root, catalog, manager) = tests::fixture(1024)?;
    let (object, path) = recording(&manager, &catalog, 1_000, true)?;
    let handle = catalog.handle();
    let job = deletion_job(&handle, &object)?;
    let (fragments, reader) =
        handle.leased_media_fragments_in_range("camera/main", 1_000, 1_100)?;
    assert_eq!(fragments.len(), 1);
    assert!(
        handle
            .execute_named_recording_deletion_authorized("admin", &job.id, &manager, |_| Ok(()))
            .is_err()
    );
    assert_eq!(std::fs::read(&path)?, b"initdata");
    assert_eq!(usage(&catalog)?, 8);
    drop(reader);
    let completed =
        handle
            .execute_named_recording_deletion_authorized("admin", &job.id, &manager, |_| Ok(()))?;
    assert_eq!(completed.deleted, 1);
    assert!(!path.exists());
    assert_eq!(usage(&catalog)?, 0);
    drop(manager);
    catalog.shutdown();
    Ok(())
}

#[test]
fn operator_deletion_resumes_staged_named_recording_after_restart() -> anyhow::Result<()> {
    let (root, catalog, manager) = tests::fixture(1024)?;
    let (object, path) = recording(&manager, &catalog, 1_000, true)?;
    let handle = catalog.handle();
    let job = deletion_job(&handle, &object)?;
    let configuration = manager.configuration().clone();
    let staged = std::cell::Cell::new(false);
    assert!(
        handle
            .execute_named_recording_deletion_authorized("admin", &job.id, &manager, |unstaged| {
                anyhow::ensure!(!staged.get(), "simulated shutdown after staging");
                if unstaged {
                    staged.set(true);
                }
                Ok(())
            })
            .is_err()
    );
    assert!(!path.exists());
    assert_eq!(usage(&catalog)?, 8);
    drop(manager);
    drop(handle);
    catalog.shutdown();
    let catalog = RecordingCatalog::open(&root.join("catalog.db"))?;
    let manager = Manager::new(configuration, catalog.handle())?;
    let report = catalog
        .handle()
        .execute_named_recording_deletion_authorized("admin", &job.id, &manager, |_| Ok(()))?;
    assert_eq!(report.deleted, 1);
    assert_eq!(usage(&catalog)?, 0);
    assert!(
        catalog
            .handle()
            .recording_deletion_claims("admin", &job.id)
            .is_err()
    );
    let repeated = catalog
        .handle()
        .execute_named_recording_deletion_authorized("admin", &job.id, &manager, |_| Ok(()))?;
    assert_eq!(repeated.deleted, 1);
    drop(manager);
    catalog.shutdown();
    Ok(())
}

#[test]
fn operator_deletion_preserves_unavailable_named_owners() -> anyhow::Result<()> {
    let (root, catalog, manager) = tests::fixture(1024)?;
    let (object, path) = recording(&manager, &catalog, 1_000, true)?;
    let handle = catalog.handle();
    let job = deletion_job(&handle, &object)?;
    let configuration = manager.configuration().clone();
    let mut read_only = configuration.clone();
    read_only.volumes[0].state = VolumeState::ReadOnly;
    drop(manager);
    let unavailable = Manager::new(read_only, catalog.handle())?;
    assert!(
        handle
            .execute_named_recording_deletion_authorized("admin", &job.id, &unavailable, |_| Ok(()))
            .is_err()
    );
    assert_eq!(std::fs::read(&path)?, b"initdata");
    assert_eq!(usage(&catalog)?, 8);
    drop(unavailable);
    std::fs::rename(root.join("primary"), root.join("offline"))?;
    let unavailable = Manager::new(configuration.clone(), catalog.handle())?;
    assert!(
        handle
            .execute_named_recording_deletion_authorized("admin", &job.id, &unavailable, |_| Ok(()))
            .is_err()
    );
    assert_eq!(usage(&catalog)?, 8);
    drop(unavailable);
    tests::create_root(&root.join("primary"))?;
    let replacement = Manager::new(configuration.clone(), catalog.handle());
    if let Ok(replacement) = replacement {
        assert!(
            handle
                .execute_named_recording_deletion_authorized(
                    "admin",
                    &job.id,
                    &replacement,
                    |_| Ok(())
                )
                .is_err()
        );
        drop(replacement);
    }
    assert_eq!(
        std::fs::read(root.join("offline").join(path.file_name().unwrap()))?,
        b"initdata"
    );
    assert_eq!(usage(&catalog)?, 8);
    std::fs::remove_dir(root.join("primary"))?;
    std::fs::rename(root.join("offline"), root.join("primary"))?;
    let restored = Manager::new(configuration, catalog.handle())?;
    let report =
        handle
            .execute_named_recording_deletion_authorized("admin", &job.id, &restored, |_| Ok(()))?;
    assert_eq!(report.deleted, 1);
    assert_eq!(usage(&catalog)?, 0);
    drop(restored);
    catalog.shutdown();
    Ok(())
}

#[test]
fn operator_deletion_uses_moved_recordings_current_named_owner() -> anyhow::Result<()> {
    let (root, catalog, initial) = tests::fixture(1024)?;
    let (object, original) = recording(&initial, &catalog, 1_000, true)?;
    let secondary = root.join("secondary");
    tests::create_root(&secondary)?;
    let mut configuration = initial.configuration().clone();
    configuration
        .volumes
        .push(tests::volume("secondary", secondary, 1024));
    configuration.placement[0].candidates = vec![super::super::VolumeId::parse("secondary")?];
    let manager = Manager::new(configuration, catalog.handle())?;
    let move_id = uuid::Uuid::new_v4().to_string();
    assert!(manager.move_object(
        &move_id,
        object.clone(),
        &PlacementRequest {
            role: VolumeRole::Active,
            source: "camera",
            group: "",
            required_bytes: 8,
        },
        &[],
        || false
    )?);
    let job = deletion_job(&catalog.handle(), &object)?;
    assert!(
        catalog
            .handle()
            .execute_named_recording_deletion_authorized("admin", &job.id, &manager, |_| Ok(()))
            .is_err()
    );
    assert!(manager.retire_move(&move_id)?);
    assert!(!original.exists());
    let Reply::Location(Some(location)) =
        catalog.handle().volume_location(Request::Lookup(object))?
    else {
        anyhow::bail!("moved recording missing");
    };
    let destination = manager.owned_path(&location)?;
    assert_eq!(std::fs::read(&destination)?, b"initdata");
    let report = catalog
        .handle()
        .execute_named_recording_deletion_authorized("admin", &job.id, &manager, |_| Ok(()))?;
    assert_eq!(report.deleted, 1);
    assert!(!destination.exists());
    assert_eq!(usage(&catalog)?, 0);
    drop(manager);
    drop(initial);
    catalog.shutdown();
    Ok(())
}

#[test]
fn moved_recording_retires_from_its_authoritative_destination() -> anyhow::Result<()> {
    let (root, catalog, initial) = tests::fixture(1024)?;
    let (object, original) = recording(&initial, &catalog, 1000, true)?;
    let secondary = root.join("secondary");
    tests::create_root(&secondary)?;
    let mut configuration = initial.inner.configuration.clone();
    configuration
        .volumes
        .push(tests::volume("secondary", secondary, 1024));
    configuration.placement[0].candidates = vec![super::super::VolumeId::parse("secondary")?];
    let manager = Manager::new(configuration, catalog.handle())?;
    let move_id = uuid::Uuid::new_v4().to_string();
    assert!(manager.move_object(
        &move_id,
        object.clone(),
        &PlacementRequest {
            role: VolumeRole::Active,
            source: "camera",
            group: "",
            required_bytes: 8,
        },
        &[],
        || false
    )?);
    assert!(manager.retire_move(&move_id)?);
    assert!(!original.exists());
    let job = begin(&catalog, "secondary")?;
    assert_eq!(job.location.object, object);
    assert_eq!(job.location.volume, "secondary");
    let destination = manager.owned_path(&job.location)?;
    assert_eq!(std::fs::read(&destination)?, b"initdata");
    assert!(manager.finish_recording_retirement(&job.operation)?);
    assert!(!destination.exists());
    assert_eq!(usage(&catalog)?, 0);
    assert_eq!(
        catalog.handle().volume_location(Request::Lookup(object))?,
        Reply::Location(None)
    );
    assert!(
        catalog
            .handle()
            .media_fragments_in_range("camera/main", 1000, 1100)?
            .is_empty()
    );
    assert!(manager.finish_recording_retirement(&job.operation)?);
    drop(manager);
    drop(initial);
    catalog.shutdown();
    Ok(())
}

#[test]
fn recording_retention_claim_is_volume_scoped_and_excludes_active_and_protected()
-> anyhow::Result<()> {
    let (root, catalog, primary) = tests::fixture(1024)?;
    let secondary_root = root.join("secondary");
    tests::create_root(&secondary_root)?;
    let mut configuration = primary.inner.configuration.clone();
    configuration
        .volumes
        .push(tests::volume("secondary", secondary_root, 1024));
    configuration.placement[0].candidates = vec![super::super::VolumeId::parse("secondary")?];
    let secondary = Manager::new(configuration, catalog.handle())?;
    let (other, other_path) = recording(&secondary, &catalog, 1, true)?;
    let (protected, protected_path) = recording(&primary, &catalog, 2, true)?;
    catalog
        .handle()
        .set_recording_protected(&protected.id, true)?;
    let (active, active_path) = recording(&primary, &catalog, 3, false)?;
    let (eligible, eligible_path) = recording(&primary, &catalog, 4, true)?;
    let job = begin(&catalog, "primary")?;
    assert_eq!(job.location.object, eligible);
    assert_eq!(job.location.volume, "primary");
    assert!(primary.finish_recording_retirement(&job.operation)?);
    assert!(!eligible_path.exists());
    assert!(other_path.exists() && protected_path.exists() && active_path.exists());
    assert!(matches!(
        catalog.handle().volume_location(Request::Lookup(other))?,
        Reply::Location(Some(_))
    ));
    assert!(matches!(
        catalog.handle().volume_location(Request::Lookup(active))?,
        Reply::Location(None)
    ));
    drop(primary);
    drop(secondary);
    catalog.shutdown();
    Ok(())
}

#[test]
fn admitted_recording_retirement_fences_new_readers_and_protection_but_waits_existing_reader()
-> anyhow::Result<()> {
    let (_root, catalog, manager) = tests::fixture(1024)?;
    let (object, path) = recording(&manager, &catalog, 1000, true)?;
    assert_eq!(catalog.handle().legacy_recording_bytes()?, 0);
    assert!(catalog.handle().claim_cleanup_candidate()?.is_none());
    let (fragments, reader) =
        catalog
            .handle()
            .leased_media_fragments_in_range("camera/main", 1000, 1100)?;
    assert_eq!(fragments.len(), 1);
    let job = begin(&catalog, "primary")?;
    assert!(catalog.handle().lease_media_fragments(&fragments).is_err());
    assert!(
        catalog
            .handle()
            .set_recording_protected(&object.id, true)
            .is_err()
    );
    assert!(manager.finish_recording_retirement(&job.operation)?);
    assert_eq!(std::fs::read(&path)?, b"initdata");
    assert_eq!(usage(&catalog)?, 8);
    drop(reader);
    assert!(manager.finish_recording_retirement(&job.operation)?);
    assert!(!path.exists());
    assert_eq!(usage(&catalog)?, 0);
    let coverage = catalog.handle().coverage(1000..1100)?;
    let deleted = &coverage.streams[0].deletions[0];
    assert_eq!((deleted.start_ms, deleted.end_ms), (1000, 1100));
    assert_eq!(
        deleted.reason,
        crate::storage::catalog::CatalogDeletionReason::ArchiveLimit
    );
    assert!(
        catalog
            .handle()
            .media_fragments_in_range("camera/main", 1000, 1100)?
            .is_empty()
    );
    assert_eq!(
        catalog.handle().volume_location(Request::Lookup(object))?,
        Reply::Location(None)
    );
    assert!(manager.finish_recording_retirement(&job.operation)?);
    let Reply::RecordingRetirement(Some(done)) =
        catalog
            .handle()
            .volume_location(Request::RecordingRetention(Action::Load(
                job.operation.clone(),
            )))?
    else {
        anyhow::bail!("completed receipt journal missing");
    };
    assert!(done.complete && done.acknowledged);
    drop(manager);
    catalog.shutdown();
    Ok(())
}

#[test]
fn recording_retirement_restart_on_read_only_volume_preserves_ownership_until_writable()
-> anyhow::Result<()> {
    let (root, catalog, manager) = tests::fixture(1024)?;
    let (object, path) = recording(&manager, &catalog, 1000, true)?;
    let job = begin(&catalog, "primary")?;
    let configuration = manager.inner.configuration.clone();
    drop(manager);
    catalog.shutdown();
    let catalog = RecordingCatalog::open(&root.join("catalog.db"))?;
    let mut read_only = configuration.clone();
    read_only.volumes[0].state = VolumeState::ReadOnly;
    let unavailable = Manager::new(read_only, catalog.handle())?;
    assert!(
        unavailable
            .finish_recording_retirement(&job.operation)
            .is_err()
    );
    assert_eq!(std::fs::read(&path)?, b"initdata");
    assert_eq!(usage(&catalog)?, 8);
    assert_eq!(
        catalog
            .handle()
            .volume_location(Request::Lookup(object.clone()))?,
        Reply::Location(Some(job.location.clone()))
    );
    drop(unavailable);
    let restored = Manager::new(configuration, catalog.handle())?;
    assert!(restored.finish_recording_retirement(&job.operation)?);
    assert!(!path.exists());
    assert_eq!(usage(&catalog)?, 0);
    assert_eq!(
        catalog.handle().volume_location(Request::Lookup(object))?,
        Reply::Location(None)
    );
    assert!(restored.finish_recording_retirement(&job.operation)?);
    drop(restored);
    catalog.shutdown();
    Ok(())
}

#[test]
fn refused_allocation_wakes_retention_then_can_use_reclaimed_capacity() -> anyhow::Result<()> {
    let (_root, catalog, manager) = tests::fixture(32)?;
    let (_recording, path) = recording(&manager, &catalog, 1000, true)?;
    let worker = worker::Worker::start(manager.clone())?;
    let object = Object {
        kind: Kind::Export,
        id: uuid::Uuid::new_v4().to_string(),
    };
    assert!(
        manager
            .reserve(VolumeRole::Export, "camera", &[], object.clone(), 32)
            .is_err()
    );
    for _ in 0..100 {
        if !path.exists() && usage(&catalog)? == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(!path.exists());
    let reservation = manager.reserve(VolumeRole::Export, "camera", &[], object, 32)?;
    assert!(reservation.is_some());
    worker.shutdown()?;
    drop(reservation);
    drop(manager);
    catalog.shutdown();
    Ok(())
}

#[test]
fn missing_root_retains_recording_journal_until_the_same_root_returns() -> anyhow::Result<()> {
    let (root, catalog, manager) = tests::fixture(1024)?;
    let (object, path) = recording(&manager, &catalog, 1000, true)?;
    let job = begin(&catalog, "primary")?;
    let configuration = manager.configuration().clone();
    drop(manager);
    std::fs::rename(root.join("primary"), root.join("offline"))?;
    let offline = Manager::new(configuration.clone(), catalog.handle())?;
    assert!(offline.finish_recording_retirement(&job.operation).is_err());
    assert_eq!(usage(&catalog)?, 8);
    assert_eq!(
        catalog
            .handle()
            .volume_location(Request::Lookup(object.clone()))?,
        Reply::Location(Some(job.location.clone()))
    );
    drop(offline);
    std::fs::rename(root.join("offline"), root.join("primary"))?;
    let restored = Manager::new(configuration, catalog.handle())?;
    assert!(restored.finish_recording_retirement(&job.operation)?);
    assert!(!path.exists());
    assert_eq!(
        catalog.handle().volume_location(Request::Lookup(object))?,
        Reply::Location(None)
    );
    drop(restored);
    catalog.shutdown();
    Ok(())
}
