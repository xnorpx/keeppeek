use super::*;
use crate::storage::catalog::locations::recording_recovery::{Action, Mode, Pending, Plan};
use crate::storage::volumes::runtime::{Reservation, ReservedFile};
use crate::storage::{CatalogMediaFragment, CatalogRecording};

struct Initialized {
    object: Object,
    operation: String,
    path: PathBuf,
    bytes: u64,
    writer: ReservedFile,
}

fn reserve(manager: &Manager) -> anyhow::Result<(Object, Reservation)> {
    let object = Object {
        kind: Kind::Recording,
        id: uuid::Uuid::new_v4().to_string(),
    };
    let reservation = manager
        .reserve(VolumeRole::Active, "camera", &[], object.clone(), 64)?
        .unwrap();
    Ok((object, reservation))
}

fn initialized(manager: &Manager, catalog: &RecordingCatalog) -> anyhow::Result<Initialized> {
    let (object, reservation) = reserve(manager)?;
    let operation = reservation.operation.clone();
    let path = reservation.path().to_path_buf();
    let MediaFrame::Video(video) = frame(Instant::now(), 0).frame else {
        unreachable!()
    };
    let config = mp4::Mp4Config {
        major_brand: "iso6".parse()?,
        minor_version: 1,
        compatible_brands: vec!["iso6".parse()?, "isom".parse()?, "mp41".parse()?],
        timescale: 1000,
    };
    let track = mp4::TrackConfig {
        track_type: mp4::TrackType::Video,
        timescale: 90_000,
        language: "und".into(),
        media_conf: crate::storage::medium_term::required_video_media_config(&video)?,
    };
    // ponytail: use the production MP4 initializer without exposing writer activation for tests.
    let writer = mp4::FragmentedMp4Writer::write_start(reservation.open()?, &config, &[track])?;
    let initialization = writer.initialization();
    catalog.handle().upsert_recording(CatalogRecording {
        id: object.id.clone(),
        stream_id: "camera/main".into(),
        source_id: Some("camera".into()),
        logical_stream_id: Some("main".into()),
        started_at_ms: 1000,
        ended_at_ms: None,
        path: path.to_string_lossy().into_owned(),
        init_offset: initialization.offset,
        init_len: initialization.size,
        finalized: false,
    })?;
    let mut writer = writer.into_writer();
    writer.evidence()?;
    assert_eq!(
        fs::metadata(&path)?.len(),
        initialization.offset + initialization.size
    );
    assert_eq!(catalog.handle().stats()?.fragments, 0);
    Ok(Initialized {
        object,
        operation,
        path,
        bytes: initialization.size,
        writer,
    })
}

fn initialization_reader(
    catalog: &RecordingCatalog,
    initialized: &Initialized,
) -> anyhow::Result<crate::storage::catalog::readers::LeaseSet> {
    catalog
        .handle()
        .lease_media_fragments(&[CatalogMediaFragment {
            recording_id: initialized.object.id.clone(),
            recording_started_at_ms: 1000,
            path: initialized.path.to_string_lossy().into_owned(),
            init_offset: 0,
            init_len: initialized.bytes,
            sequence: 0,
            start_ms: 1000,
            duration_ms: 0,
            byte_offset: 0,
            byte_len: initialized.bytes,
        }])
}

#[test]
fn captured_empty_recording_is_reclaimed_only_after_its_writer_closes() -> anyhow::Result<()> {
    let (root, catalog, manager) = tests::fixture(2 * 1_048_576)?;
    let (object, reservation) = reserve(&manager)?;
    let operation = reservation.operation.clone();
    let path = reservation.path().to_path_buf();
    let writer = reservation.open()?;
    let unrelated = path.parent().unwrap().join("unrelated.mp4");
    fs::write(&unrelated, b"unrelated recording")?;
    let _attempt = manager.recover_pending_recording(&operation);
    assert_eq!(fs::metadata(&path)?.len(), 0);
    assert_eq!(allocated(&catalog)?, 64);
    assert_eq!(catalog.handle().stats()?.recording_files, 0);
    drop(writer);
    assert!(manager.recover_pending_recording(&operation)?);
    assert!(!path.exists());
    assert_eq!(allocated(&catalog)?, 0);
    assert_eq!(location(&catalog, &object)?, None);
    assert_eq!(fs::read(&unrelated)?, b"unrelated recording");
    manager.recover_pending_recording(&operation)?;
    assert!(!path.exists());
    assert_eq!(allocated(&catalog)?, 0);
    drop(manager);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn initialization_only_recording_waits_for_writer_and_reader_then_releases_ownership()
-> anyhow::Result<()> {
    let (root, catalog, manager) = tests::fixture(2 * 1_048_576)?;
    let initialized = initialized(&manager, &catalog)?;
    let before = fs::read(&initialized.path)?;
    let allocated_before = allocated(&catalog)?;
    let lease = initialization_reader(&catalog, &initialized)?;
    let _attempt = manager.recover_pending_recording(&initialized.operation);
    assert_eq!(fs::read(&initialized.path)?, before);
    assert_eq!(allocated(&catalog)?, allocated_before);
    drop(initialized.writer);
    let _attempt = manager.recover_pending_recording(&initialized.operation);
    assert_eq!(fs::read(&initialized.path)?, before);
    assert_eq!(allocated(&catalog)?, allocated_before);
    assert_eq!(catalog.handle().stats()?.recording_files, 1);
    drop(lease);
    assert!(manager.recover_pending_recording(&initialized.operation)?);
    assert!(!initialized.path.exists());
    assert_eq!(allocated(&catalog)?, 0);
    assert_eq!(catalog.handle().stats()?.recording_files, 0);
    assert_eq!(location(&catalog, &initialized.object)?, None);
    manager.recover_pending_recording(&initialized.operation)?;
    assert_eq!(allocated(&catalog)?, 0);
    drop(manager);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn initialization_with_unindexed_bytes_is_preserved_with_its_reservation() -> anyhow::Result<()> {
    let (root, catalog, manager) = tests::fixture(2 * 1_048_576)?;
    let initialized = initialized(&manager, &catalog)?;
    drop(initialized.writer);
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&initialized.path)?;
    file.write_all(b"\0\0\0\x40moofunindexed recording payload")?;
    file.sync_all()?;
    drop(file);
    let before = fs::read(&initialized.path)?;
    let allocated_before = allocated(&catalog)?;
    assert!(
        manager
            .recover_pending_recording(&initialized.operation)
            .is_err()
    );
    assert_eq!(fs::read(&initialized.path)?, before);
    assert_eq!(allocated(&catalog)?, allocated_before);
    assert_eq!(catalog.handle().stats()?.recording_files, 1);
    assert_eq!(catalog.handle().stats()?.fragments, 0);
    assert_eq!(catalog.handle().stats()?.finalized_files, 0);
    assert_eq!(location(&catalog, &initialized.object)?, None);
    drop(manager);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn unidentified_file_at_unopened_recording_path_preserves_bytes_and_reservation()
-> anyhow::Result<()> {
    let (root, catalog, manager) = tests::fixture(2 * 1_048_576)?;
    let (object, reservation) = reserve(&manager)?;
    let operation = reservation.operation.clone();
    let path = reservation.path().to_path_buf();
    fs::write(&path, b"unidentified existing recording")?;
    drop(reservation);
    assert!(manager.recover_pending_recording(&operation).is_err());
    assert_eq!(fs::read(&path)?, b"unidentified existing recording");
    assert_eq!(allocated(&catalog)?, 64);
    assert_eq!(catalog.handle().stats()?.recording_files, 0);
    assert_eq!(location(&catalog, &object)?, None);
    drop(manager);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

fn alias_recording(id: &str, path: &Path) -> CatalogRecording {
    let original = path.to_string_lossy();
    let alias = if original.contains('\\') {
        original.replace('\\', "/")
    } else {
        original.replace('/', "\\")
    };
    assert_ne!(alias, original);
    CatalogRecording {
        id: id.into(),
        stream_id: format!("{id}/main"),
        source_id: None,
        logical_stream_id: None,
        started_at_ms: 1000,
        ended_at_ms: Some(2000),
        path: alias,
        init_offset: 0,
        init_len: 0,
        finalized: true,
    }
}

fn begin_abandonment(
    catalog: &RecordingCatalog,
    manager: &Manager,
    operation: &str,
) -> anyhow::Result<(Box<Pending>, Plan)> {
    let handle = catalog.handle();
    let _worker = handle.claim_volume_move(operation)?;
    let Reply::PendingRecording(Some(pending)) =
        handle.volume_location(Request::RecordingRecovery(Action::Load(operation.into())))?
    else {
        anyhow::bail!("pending abandonment missing")
    };
    let plan = manager.begin_recording_recovery(&pending)?;
    assert_eq!(plan.mode, Mode::Abandon);
    Ok((pending, plan))
}

#[test]
fn protected_alias_metadata_prevents_initialization_abandonment() -> anyhow::Result<()> {
    let (root, catalog, manager) = tests::fixture(2 * 1_048_576)?;
    let initialized = initialized(&manager, &catalog)?;
    drop(initialized.writer);
    let handle = catalog.handle();
    handle.upsert_recording(alias_recording("alias", &initialized.path))?;
    handle.insert_fragment(crate::storage::CatalogFragment {
        recording_id: "alias".into(),
        sequence: 1,
        start_ms: 1000,
        duration_ms: 1000,
        byte_offset: 0,
        byte_len: initialized.bytes,
        random_access: true,
    })?;
    handle.set_recording_protected("alias", true)?;
    let before = fs::read(&initialized.path)?;
    let allocated_before = allocated(&catalog)?;
    assert!(
        manager
            .recover_pending_recording(&initialized.operation)
            .is_err()
    );
    assert_eq!(fs::read(&initialized.path)?, before);
    assert_eq!(allocated(&catalog)?, allocated_before);
    assert_eq!(handle.stats()?.recording_files, 2);
    assert_eq!(handle.stats()?.fragments, 1);
    assert_eq!(handle.stats()?.protected_files, 1);
    assert_eq!(location(&catalog, &initialized.object)?, None);
    drop(handle);
    drop(manager);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn committed_abandonment_rejects_new_alias_rows_and_alias_path_updates() -> anyhow::Result<()> {
    let (root, catalog, manager) = tests::fixture(2 * 1_048_576)?;
    let initialized = initialized(&manager, &catalog)?;
    drop(initialized.writer);
    let other = root.join("other.mp4");
    fs::write(&other, b"other recording")?;
    let handle = catalog.handle();
    let mut existing = alias_recording("existing", &other);
    handle.upsert_recording(existing.clone())?;
    let before = fs::read(&initialized.path)?;
    let allocated_before = allocated(&catalog)?;
    begin_abandonment(&catalog, &manager, &initialized.operation)?;
    assert!(
        handle
            .upsert_recording(alias_recording("new-alias", &initialized.path))
            .is_err()
    );
    existing.path = alias_recording("unused", &initialized.path).path;
    assert!(handle.upsert_recording(existing).is_err());
    assert_eq!(handle.stats()?.recording_files, 2);
    assert_eq!(fs::read(&initialized.path)?, before);
    assert_eq!(allocated(&catalog)?, allocated_before);
    assert!(manager.recover_pending_recording(&initialized.operation)?);
    assert!(!initialized.path.exists());
    assert_eq!(allocated(&catalog)?, 0);
    assert_eq!(handle.stats()?.recording_files, 1);
    assert_eq!(fs::read(&other)?, b"other recording");
    drop(handle);
    drop(manager);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

fn retire_without_acknowledgement(
    catalog: &RecordingCatalog,
    manager: &Manager,
    pending: &Pending,
    plan: &Plan,
    complete: bool,
) -> anyhow::Result<()> {
    let _worker = catalog
        .handle()
        .claim_volume_move(&plan.evidence.operation)?;
    manager.inner.writable_root(0)?.retire_owned(
        &pending.owned.relative_key,
        &plan.evidence.file_identity,
        plan.evidence.bytes,
        plan.evidence.digest,
        &plan.evidence.operation,
    )?;
    if complete {
        assert_eq!(
            catalog
                .handle()
                .volume_location(Request::RecordingRecovery(Action::Complete(
                    plan.evidence.clone()
                ),))?,
            Reply::Bound
        );
    }
    Ok(())
}

fn assert_abandonment_retry(
    catalog: &RecordingCatalog,
    manager: &Manager,
    plan: Plan,
) -> anyhow::Result<()> {
    assert_eq!(
        catalog
            .handle()
            .volume_location(Request::RecordingRecovery(Action::Complete(
                plan.evidence.clone()
            ),))?,
        Reply::Bound
    );
    manager.recover_pending_recording(&plan.evidence.operation)?;
    let mut changed = plan.evidence;
    changed.digest[0] ^= 1;
    assert!(
        catalog
            .handle()
            .volume_location(Request::RecordingRecovery(Action::Complete(changed)))
            .is_err()
    );
    assert_eq!(allocated(catalog)?, 0);
    Ok(())
}

#[test]
fn abandonment_restarts_after_file_removal_and_after_catalog_completion() -> anyhow::Result<()> {
    for complete_before_restart in [false, true] {
        let (root, catalog, manager) = tests::fixture(2 * 1_048_576)?;
        let initialized = initialized(&manager, &catalog)?;
        drop(initialized.writer);
        let allocated_before = allocated(&catalog)?;
        let (pending, plan) = begin_abandonment(&catalog, &manager, &initialized.operation)?;
        retire_without_acknowledgement(
            &catalog,
            &manager,
            &pending,
            &plan,
            complete_before_restart,
        )?;
        assert!(!initialized.path.exists());
        let receipt = initialized
            .path
            .parent()
            .unwrap()
            .join(".retired")
            .join(&initialized.operation);
        assert!(receipt.is_dir());
        let expected_bytes = if complete_before_restart {
            0
        } else {
            allocated_before
        };
        assert_eq!(allocated(&catalog)?, expected_bytes);
        let configuration = manager.configuration().clone();
        drop(manager);
        catalog.shutdown();
        let catalog = RecordingCatalog::open(&root.join("catalog.db"))?;
        let manager = Manager::new(configuration, catalog.handle())?;
        let Reply::PendingRecording(Some(recovered)) =
            catalog
                .handle()
                .volume_location(Request::RecordingRecovery(Action::Load(
                    initialized.operation.clone(),
                )))?
        else {
            anyhow::bail!("abandonment recovery disappeared")
        };
        assert_eq!(recovered.plan, Some(plan.clone()));
        assert_eq!(recovered.complete, complete_before_restart);
        assert_eq!(allocated(&catalog)?, expected_bytes);
        assert!(manager.recover_pending_recording(&initialized.operation)?);
        assert!(!initialized.path.exists());
        assert!(!receipt.exists());
        assert_eq!(allocated(&catalog)?, 0);
        assert_eq!(catalog.handle().stats()?.recording_files, 0);
        assert_eq!(location(&catalog, &initialized.object)?, None);
        assert_abandonment_retry(&catalog, &manager, plan)?;
        drop(manager);
        catalog.shutdown();
        fs::remove_dir_all(root)?;
    }
    Ok(())
}
