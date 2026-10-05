use super::*;
use crate::storage::{
    RecordingCatalog,
    catalog::locations::{Kind, Object},
    identity::RecordingStreamIdentity,
    medium_term::MediumTermWriter,
    volumes::{PlacementRule, PlacementStrategy, VolumeConfiguration, VolumeId, VolumeRole},
};
use std::path::PathBuf;

fn fixture() -> anyhow::Result<(PathBuf, RecordingCatalog, Manager, VolumeConfiguration)> {
    let (path, catalog, initial) = super::super::tests::fixture(4 * 1_048_576)?;
    let mut configuration = initial.inner.configuration.clone();
    let mut archive = super::super::tests::volume("archive", path.join("archive"), 4 * 1_048_576);
    archive.roles = vec![VolumeRole::Archive];
    configuration.volumes.push(archive);
    configuration.placement.push(PlacementRule {
        role: VolumeRole::Archive,
        source: None,
        group: None,
        candidates: vec![VolumeId::parse("archive")?],
        strategy: PlacementStrategy::Priority,
        allow_fallback: false,
    });
    let manager = Manager::new(configuration.clone(), catalog.handle())?;
    Ok((path, catalog, manager, configuration))
}

fn writer(manager: &Manager) -> anyhow::Result<(Object, MediumTermWriter)> {
    let object = Object {
        kind: Kind::Recording,
        id: uuid::Uuid::new_v4().to_string(),
    };
    let reservation = manager
        .reserve(VolumeRole::Active, "camera", &[], object.clone(), 1)?
        .expect("active recording reservation");
    let start = Instant::now();
    let mut writer = MediumTermWriter::create_with_reservation(
        reservation,
        object.id.clone(),
        RecordingStreamIdentity::legacy("camera/main"),
        start,
        8192,
        manager.inner.catalog.clone(),
    )?;
    for milliseconds in [0, 100] {
        writer.append_one(frame(start, milliseconds))?;
    }
    Ok((object, writer))
}

fn frame(start: Instant, milliseconds: u64) -> crate::storage::RecordingFrame {
    use crate::storage::{MediaFrame, RecordingFrame, VideoCodec, VideoFrame};
    let elapsed = Duration::from_millis(milliseconds);
    RecordingFrame {
        received_at: start + elapsed,
        timestamp: Some(elapsed),
        frame: MediaFrame::Video(VideoFrame {
            codec: VideoCodec::H264,
            is_keyframe: true,
            width: 320,
            height: 240,
            data: bytes::Bytes::from_static(&[
                0, 0, 0, 8, 0x67, 0x42, 0x00, 0x1f, 0xe5, 0x88, 0x68, 0x40, 0, 0, 0, 4, 0x68, 0xce,
                0x3c, 0x80, 0, 0, 0, 1, 0x65,
            ]),
        }),
    }
}

#[test]
fn archive_request_survives_offline_destination_restart_and_default_change() -> anyhow::Result<()> {
    let (path, catalog, manager, mut configuration) = fixture()?;
    let (object, writer) = writer(&manager)?;
    let recovery = Scan::new()
        .next(&manager)?
        .expect("active reservation is scanned for recovery");
    assert!(execute(&manager, &recovery, &AtomicBool::new(false)).is_err());
    assert_eq!(
        manager
            .inner
            .catalog
            .volume_location(Request::Lookup(object.clone()))?,
        Reply::Location(None)
    );
    let source_path = writer.finalize()?;
    let original = std::fs::read(&source_path)?;
    let id = Scan::new()
        .next(&manager)?
        .expect("finalization makes archive request runnable");
    assert!(execute(&manager, &id, &AtomicBool::new(false)).is_err());
    assert_eq!(std::fs::read(&source_path)?, original);
    drop(manager);
    catalog.shutdown();

    configuration
        .placement
        .retain(|rule| rule.role != VolumeRole::Archive);
    super::super::tests::create_root(&path.join("archive"))?;
    let catalog = RecordingCatalog::open(&path.join("catalog.db"))?;
    let manager = Manager::new(configuration, catalog.handle())?;
    assert_eq!(Scan::new().next(&manager)?, Some(id.clone()));
    execute(&manager, &id, &AtomicBool::new(false))?;
    let Reply::Location(Some(location)) =
        catalog.handle().volume_location(Request::Lookup(object))?
    else {
        anyhow::bail!("archived recording disappeared");
    };
    assert_eq!(location.volume, "archive");
    assert_eq!(
        std::fs::read(path.join("archive").join(location.relative_key))?,
        original
    );
    assert!(!source_path.exists());
    assert_eq!(Scan::new().next(&manager)?, None);
    catalog.shutdown();
    Ok(())
}

#[test]
fn refused_archive_admission_keeps_the_request_for_another_pass() -> anyhow::Result<()> {
    let (path, catalog, manager, mut configuration) = fixture()?;
    super::super::tests::create_root(&path.join("archive"))?;
    manager.recover_roots()?;
    let (object, writer) = writer(&manager)?;
    let source_path = writer.finalize()?;
    let id = Scan::new().next(&manager)?.expect("pending archive");
    configuration.volumes[1].state = crate::storage::volumes::VolumeState::ReadOnly;
    let read_only = Manager::new(configuration.clone(), catalog.handle())?;
    // The older manager can select the destination, but catalog admission must still refuse it.
    assert!(execute(&manager, &id, &AtomicBool::new(false)).is_err());
    assert_eq!(Scan::new().next(&manager)?, Some(id.clone()));
    assert!(
        catalog
            .handle()
            .volume_location(Request::Move(id.clone()))
            .is_err()
    );
    assert!(source_path.exists());
    drop(read_only);
    configuration.volumes[1].state = crate::storage::volumes::VolumeState::Enabled;
    let available = Manager::new(configuration, catalog.handle())?;
    execute(&available, &id, &AtomicBool::new(false))?;
    let Reply::Location(Some(location)) =
        catalog.handle().volume_location(Request::Lookup(object))?
    else {
        anyhow::bail!("archived recording disappeared");
    };
    assert_eq!(location.volume, "archive");
    assert!(!source_path.exists());
    catalog.shutdown();
    Ok(())
}

#[test]
fn archive_already_on_its_only_destination_needs_no_extra_capacity() -> anyhow::Result<()> {
    let (_path, catalog, original, mut configuration) = fixture()?;
    configuration.volumes[0].roles.push(VolumeRole::Archive);
    configuration.placement.last_mut().unwrap().candidates = vec![VolumeId::parse("primary")?];
    let manager = Manager::new(configuration.clone(), catalog.handle())?;
    let (_object, writer) = writer(&manager)?;
    let path = writer.finalize()?;
    let bytes = std::fs::read(&path)?;
    let id = Scan::new().next(&manager)?.expect("pending archive");
    configuration.volumes[0].capacity_bytes = Some(u64::try_from(bytes.len())?);
    let full = Manager::new(configuration, catalog.handle())?;
    execute(&full, &id, &AtomicBool::new(false))?;
    assert_eq!(std::fs::read(path)?, bytes);
    assert_eq!(Scan::new().next(&full)?, None);
    assert!(catalog.handle().volume_location(Request::Move(id)).is_err());
    drop(original);
    catalog.shutdown();
    Ok(())
}

fn allocation(manager: &Manager) -> anyhow::Result<crate::storage::catalog::locations::Allocation> {
    let id = uuid::Uuid::new_v4().to_string();
    Ok(crate::storage::catalog::locations::Allocation {
        operation: uuid::Uuid::new_v4().to_string(),
        object: Object {
            kind: Kind::Recording,
            id: id.clone(),
        },
        volume: "primary".into(),
        generation: 1,
        relative_key: format!("{id}.mp4"),
        bytes: 1,
        capacity: manager
            .inner
            .root(0)?
            .capacity(manager.inner.catalog.volume_ledger_revision()?)?,
    })
}

#[test]
fn archive_reservation_retry_is_exact_and_failed_enqueue_rolls_back() -> anyhow::Result<()> {
    use crate::storage::catalog::locations::archives::Intent;
    let (path, catalog, manager, _configuration) = fixture()?;
    let handle = catalog.handle();
    let initial = allocation(&manager)?;
    let intent = Intent {
        id: uuid::Uuid::new_v4().to_string(),
        policy: manager.archive_policy("camera", &[]).unwrap(),
    };
    let request = Request::ReserveArchive(initial.clone(), intent.clone());
    let reply = handle.volume_location(request.clone())?;
    let revision = handle.volume_ledger_revision()?;
    assert_eq!(handle.volume_location(request.clone())?, reply);
    assert_eq!(handle.volume_ledger_revision()?, revision);
    let mut changed = intent.clone();
    changed.policy.configuration.placement[0].allow_fallback = true;
    assert!(
        handle
            .volume_location(Request::ReserveArchive(initial, changed))
            .is_err()
    );
    let usage = handle.volume_location(Request::Usage)?;
    assert!(
        handle
            .volume_location(Request::ReserveArchive(allocation(&manager)?, intent))
            .is_err()
    );
    assert_eq!(handle.volume_location(Request::Usage)?, usage);
    assert_eq!(handle.volume_ledger_revision()?, revision);
    drop(manager);
    catalog.shutdown();
    let catalog = RecordingCatalog::open(&path.join("catalog.db"))?;
    assert_eq!(catalog.handle().volume_location(request)?, reply);
    catalog.shutdown();
    Ok(())
}

#[test]
fn finalized_recording_wakes_the_existing_archive_worker() -> anyhow::Result<()> {
    let (path, catalog, manager, _configuration) = fixture()?;
    super::super::tests::create_root(&path.join("archive"))?;
    manager.recover_roots()?;
    let (object, writer) = writer(&manager)?;
    let worker = Worker::start(manager)?;
    let source_path = writer.finalize()?;
    worker.handle().scan()?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let Reply::Location(Some(location)) = catalog
            .handle()
            .volume_location(Request::Lookup(object.clone()))?
        else {
            anyhow::bail!("finalized recording disappeared");
        };
        if location.volume == "archive" {
            break;
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "archive worker did not publish the recording"
        );
        thread::sleep(Duration::from_millis(10));
    }
    worker.shutdown()?;
    assert!(path.join("archive").read_dir()?.next().is_some());
    // Shutdown can precede source retirement; a retained source is safe and stays journaled.
    if source_path.exists() {
        assert!(!std::fs::read(source_path)?.is_empty());
    }
    catalog.shutdown();
    Ok(())
}

#[test]
fn adding_an_archive_default_does_not_move_existing_recordings() -> anyhow::Result<()> {
    let (path, catalog, _original, configuration) = fixture()?;
    let mut without_archive = configuration.clone();
    without_archive
        .placement
        .retain(|rule| rule.role != VolumeRole::Archive);
    let manager = Manager::new(without_archive, catalog.handle())?;
    let (object, writer) = writer(&manager)?;
    let source_path = writer.finalize()?;
    super::super::tests::create_root(&path.join("archive"))?;
    let changed = Manager::new(configuration, catalog.handle())?;
    assert_eq!(Scan::new().next(&changed)?, None);
    let Reply::Location(Some(location)) =
        catalog.handle().volume_location(Request::Lookup(object))?
    else {
        anyhow::bail!("existing recording disappeared");
    };
    assert_eq!(location.volume, "primary");
    assert!(source_path.exists());
    assert!(path.join("archive").read_dir()?.next().is_none());
    catalog.shutdown();
    Ok(())
}
