use super::*;
use crate::storage::catalog::{
    CatalogFragment, CatalogRecording, RecordingCatalog,
    locations::{
        legacy::{LegacyPaths, adoption, inventory, roots},
        moves,
    },
};

fn legacy_move_fixture() -> anyhow::Result<(PathBuf, RecordingCatalog, Manager, adoption::Intent)> {
    let (base, catalog, manager) = tests::fixture(4 * GROWTH_BYTES)?;
    let media = base.join("legacy");
    tests::create_root(&media)?;
    std::fs::create_dir(media.join("camera"))?;
    let path = media.join("camera/non-uuid-recording.mp4");
    std::fs::write(&path, [7_u8; 128])?;
    let paths = LegacyPaths {
        active_root: media.clone(),
        archive_root: media.clone(),
        export_root: media.join(".exports"),
        thumbnail_root: media.join("images"),
        catalog_path: base.join("catalog.db"),
        export_history_path: media.join(".exports/history.json"),
    };
    crate::storage::volumes::legacy::capture_roots(&catalog.handle(), &paths)?;
    seed_legacy_recording(&catalog, &path)?;
    let Reply::LegacyReferences(mut references) = catalog.handle().volume_location(
        Request::LegacyInventory(inventory::Action::Recordings {
            after: None,
            limit: 1,
        }),
    )?
    else {
        anyhow::bail!("legacy reference missing")
    };
    assert_eq!(references.len(), 1);
    let reference = crate::storage::volumes::legacy::verify_recording(
        &catalog.handle(),
        &references.remove(0),
    )?;
    let intent = adoption_intent(&manager, &catalog, reference)?;
    Ok((base, catalog, manager, intent))
}

fn seed_legacy_recording(catalog: &RecordingCatalog, path: &Path) -> anyhow::Result<()> {
    catalog.handle().upsert_recording(CatalogRecording {
        id: "legacy-recording".into(),
        stream_id: "camera/main".into(),
        source_id: Some("camera".into()),
        logical_stream_id: Some("main".into()),
        started_at_ms: 1000,
        ended_at_ms: Some(2000),
        path: path.to_string_lossy().into_owned(),
        init_offset: 0,
        init_len: 8,
        finalized: true,
    })?;
    catalog.handle().insert_fragment(CatalogFragment {
        recording_id: "legacy-recording".into(),
        sequence: 1,
        start_ms: 1000,
        duration_ms: 1000,
        byte_offset: 8,
        byte_len: 120,
        random_access: true,
    })?;
    Ok(())
}

fn adoption_intent(
    manager: &Manager,
    catalog: &RecordingCatalog,
    reference: inventory::Reference,
) -> anyhow::Result<adoption::Intent> {
    let id = uuid::Uuid::new_v4().to_string();
    let destination = moves::Intent {
        id: id.clone(),
        object: reference.object.clone(),
        expected_revision: 1,
        destination: Allocation {
            operation: id.clone(),
            object: Object {
                kind: Kind::Recording,
                id: id.clone(),
            },
            volume: "primary".into(),
            generation: 1,
            relative_key: format!("{id}.mp4"),
            bytes: reference.evidence.as_ref().unwrap().bytes,
            capacity: manager
                .inner
                .root(0)?
                .capacity(catalog.handle().volume_ledger_revision()?)?,
        },
    };
    Ok(adoption::Intent {
        reference,
        role: roots::Role::Active,
        operation: uuid::Uuid::new_v4().to_string(),
        destination,
    })
}

#[test]
fn adopted_nested_recording_move_waits_for_pre_adoption_reader_before_retirement()
-> anyhow::Result<()> {
    let (base, catalog, manager, intent) = legacy_move_fixture()?;
    let handle = catalog.handle();
    let (old, reader) = handle.leased_media_fragments_in_range("camera/main", 1000, 2000)?;
    assert_eq!(old.len(), 1);
    let source = intent.reference.path.clone();
    let job_id = intent.destination.id.clone();
    let destination = base
        .join("primary")
        .join(&intent.destination.destination.relative_key);
    handle.volume_location(Request::AdoptLegacyRecording(Box::new(intent)))?;
    manager.resume_move(&job_id, || false)?;
    assert_eq!(std::fs::read(&source)?, [7_u8; 128]);
    assert_eq!(std::fs::read(&destination)?, [7_u8; 128]);
    assert!(!manager.retire_move(&job_id)?);
    assert!(source.exists());
    drop(reader);
    assert!(manager.retire_move(&job_id)?);
    assert!(!source.exists());
    let current = handle.media_fragments_in_range("camera/main", 1000, 2000)?;
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].recording_id, "legacy-recording");
    assert_eq!(Path::new(&current[0].path), destination);
    assert_eq!(std::fs::read(destination)?, [7_u8; 128]);
    assert!(manager.retire_move(&job_id)?);
    drop(handle);
    drop(manager);
    catalog.shutdown();
    std::fs::remove_dir_all(base)?;
    Ok(())
}

#[test]
fn cancelling_adoption_before_copy_preserves_owned_source_for_normal_later_move()
-> anyhow::Result<()> {
    let (base, catalog, manager, intent) = legacy_move_fixture()?;
    let handle = catalog.handle();
    let source = intent.reference.path.clone();
    let object = intent.reference.object.clone();
    let cancelled = intent.destination.id.clone();
    let abandoned_path = base
        .join("primary")
        .join(&intent.destination.destination.relative_key);
    handle.volume_location(Request::AdoptLegacyRecording(Box::new(intent)))?;
    manager.cancel_move(&cancelled)?;
    assert_eq!(std::fs::read(&source)?, [7_u8; 128]);
    assert!(!abandoned_path.exists());
    let Reply::Location(Some(owned)) = handle.volume_location(Request::Lookup(object.clone()))?
    else {
        anyhow::bail!("adopted source ownership missing")
    };
    assert_eq!(owned.volume, "legacy-active");
    assert_eq!(owned.relative_key, "camera/non-uuid-recording.mp4");
    let next = uuid::Uuid::new_v4().to_string();
    assert!(manager.move_object(
        &next,
        object.clone(),
        &PlacementRequest {
            role: VolumeRole::Active,
            source: "camera",
            group: "",
            required_bytes: 128,
        },
        &[],
        || false
    )?);
    assert!(manager.retire_move(&next)?);
    assert!(!source.exists());
    let Reply::Location(Some(current)) = handle.volume_location(Request::Lookup(object.clone()))?
    else {
        anyhow::bail!("moved ownership missing")
    };
    assert_eq!(current.object, object);
    assert_eq!(current.volume, "primary");
    assert_eq!(std::fs::read(manager.owned_path(&current)?)?, [7_u8; 128]);
    drop(handle);
    drop(manager);
    catalog.shutdown();
    std::fs::remove_dir_all(base)?;
    Ok(())
}

use crate::storage::catalog::locations::recordings::{
    Action as RetentionAction, Job as RetentionJob, Reason as RetentionReason,
};

fn begin_adopted_retention(
    catalog: &RecordingCatalog,
) -> anyhow::Result<Option<Box<RetentionJob>>> {
    let Reply::RecordingRetirement(job) =
        catalog
            .handle()
            .volume_location(Request::RecordingRetention(RetentionAction::Begin {
                volume: "legacy-active".into(),
                reason: RetentionReason::Capacity,
            }))?
    else {
        anyhow::bail!("retirement reply missing")
    };
    Ok(job)
}

fn cancel_adopted_move(
    manager: &Manager,
    catalog: &RecordingCatalog,
    id: &str,
) -> anyhow::Result<()> {
    manager.cancel_move(id)?;
    let Reply::Move(job) = catalog.handle().volume_location(Request::Move(id.into()))? else {
        anyhow::bail!("cancelled move missing")
    };
    assert_eq!(job.phase, "cancelled");
    assert!(job.receipt_acknowledged);
    Ok(())
}

#[test]
fn cancelled_adoption_retains_legacy_budget_and_reader_blocks_nested_source_removal()
-> anyhow::Result<()> {
    let (base, catalog, manager, intent) = legacy_move_fixture()?;
    let handle = catalog.handle();
    let source = intent.reference.path.clone();
    let object = intent.reference.object.clone();
    let id = intent.destination.id.clone();
    handle.volume_location(Request::AdoptLegacyRecording(Box::new(intent)))?;
    cancel_adopted_move(&manager, &catalog, &id)?;
    assert_eq!(handle.legacy_recording_bytes()?, 128);
    let (fragments, reader) = handle.leased_media_fragments_in_range("camera/main", 1000, 2000)?;
    assert_eq!(fragments.len(), 1);
    let job = begin_adopted_retention(&catalog)?.expect("adopted source eligible");
    assert_eq!(job.location.object, object);
    assert_eq!(job.location.relative_key, "camera/non-uuid-recording.mp4");
    assert!(handle.lease_media_fragments(&fragments).is_err());
    manager.finish_recording_retirement(&job.operation)?;
    assert_eq!(std::fs::read(&source)?, [7_u8; 128]);
    assert_eq!(handle.legacy_recording_bytes()?, 128);
    drop(reader);
    manager.finish_recording_retirement(&job.operation)?;
    assert!(!source.exists());
    assert_eq!(handle.legacy_recording_bytes()?, 0);
    assert_eq!(
        handle.volume_location(Request::Lookup(object))?,
        Reply::Location(None)
    );
    assert!(
        handle
            .media_fragments_in_range("camera/main", 1000, 2000)?
            .is_empty()
    );
    manager.finish_recording_retirement(&job.operation)?;
    let Reply::RecordingRetirement(Some(done)) = handle.volume_location(
        Request::RecordingRetention(RetentionAction::Load(job.operation)),
    )?
    else {
        anyhow::bail!("completed retirement missing")
    };
    assert!(done.complete && done.acknowledged);
    drop(handle);
    drop(manager);
    catalog.shutdown();
    std::fs::remove_dir_all(base)?;
    Ok(())
}

#[test]
fn protected_adopted_source_is_excluded_from_legacy_pressure_retention() -> anyhow::Result<()> {
    let (base, catalog, manager, intent) = legacy_move_fixture()?;
    let handle = catalog.handle();
    let source = intent.reference.path.clone();
    let object = intent.reference.object.clone();
    let id = intent.destination.id.clone();
    handle.volume_location(Request::AdoptLegacyRecording(Box::new(intent)))?;
    cancel_adopted_move(&manager, &catalog, &id)?;
    handle.set_recording_protected(&object.id, true)?;
    let before = handle.volume_location(Request::Lookup(object.clone()))?;
    assert!(begin_adopted_retention(&catalog)?.is_none());
    assert_eq!(std::fs::read(&source)?, [7_u8; 128]);
    assert_eq!(handle.legacy_recording_bytes()?, 128);
    assert_eq!(
        handle.volume_location(Request::Lookup(object.clone()))?,
        before
    );
    handle.set_recording_protected(&object.id, false)?;
    let job = begin_adopted_retention(&catalog)?.expect("unprotected adopted source eligible");
    assert_eq!(job.location.object, object);
    manager.finish_recording_retirement(&job.operation)?;
    assert!(!source.exists());
    drop(handle);
    drop(manager);
    catalog.shutdown();
    std::fs::remove_dir_all(base)?;
    Ok(())
}

#[test]
fn active_adoption_move_excludes_legacy_source_until_cancellation_is_acknowledged()
-> anyhow::Result<()> {
    let (base, catalog, manager, intent) = legacy_move_fixture()?;
    let handle = catalog.handle();
    let source = intent.reference.path.clone();
    let id = intent.destination.id.clone();
    handle.volume_location(Request::AdoptLegacyRecording(Box::new(intent)))?;
    assert!(begin_adopted_retention(&catalog)?.is_none());
    assert_eq!(std::fs::read(&source)?, [7_u8; 128]);
    handle.volume_location(Request::AdvanceMove(moves::Step::Cancel(id.clone())))?;
    assert!(begin_adopted_retention(&catalog)?.is_none());
    cancel_adopted_move(&manager, &catalog, &id)?;
    assert!(begin_adopted_retention(&catalog)?.is_some());
    assert_eq!(std::fs::read(&source)?, [7_u8; 128]);
    drop(handle);
    drop(manager);
    catalog.shutdown();
    std::fs::remove_dir_all(base)?;
    Ok(())
}
