use super::worker::Worker;
use super::{Kind, Manager, Object, Reply, Request, Reservation, VolumeRole, tests};
use crate::storage::RecordingCatalog;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

fn fixture() -> anyhow::Result<(PathBuf, RecordingCatalog, Manager)> {
    let (root, catalog, initial) = tests::fixture(1024)?;
    let mut configuration = initial.configuration().clone();
    configuration.volumes[0].roles.push(VolumeRole::Thumbnail);
    let mut policy = configuration.placement[0].clone();
    policy.role = VolumeRole::Thumbnail;
    configuration.placement.push(policy);
    let manager = Manager::new(configuration, catalog.handle())?;
    Ok((root, catalog, manager))
}

fn reserve(manager: &Manager) -> anyhow::Result<Reservation> {
    manager
        .reserve(
            VolumeRole::Thumbnail,
            "camera",
            &[],
            Object {
                kind: Kind::Thumbnail,
                id: uuid::Uuid::new_v4().to_string(),
            },
            64,
        )?
        .ok_or_else(|| anyhow::anyhow!("thumbnail fixture policy must match"))
}

fn allocated(catalog: &RecordingCatalog) -> anyhow::Result<u64> {
    let Reply::Usage(usage) = catalog.handle().volume_location(Request::Usage)? else {
        anyhow::bail!("volume usage reply missing");
    };
    Ok(usage.iter().map(|volume| volume.allocated_bytes).sum())
}

fn wait_until_reclaimed(
    worker: &Worker,
    catalog: &RecordingCatalog,
    path: &Path,
) -> anyhow::Result<()> {
    for _ in 0..100 {
        worker.handle().scan()?;
        if !path.exists() && allocated(catalog)? == 0 {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    anyhow::ensure!(!path.exists(), "interrupted thumbnail remains on disk");
    anyhow::ensure!(
        allocated(catalog)? == 0,
        "interrupted thumbnail quota remains allocated"
    );
    Ok(())
}

#[test]
fn worker_preserves_live_thumbnail_then_recovers_dropped_partial_writer() -> anyhow::Result<()> {
    let (_root, catalog, manager) = fixture()?;
    let reservation = reserve(&manager)?;
    let path = reservation.path().to_path_buf();
    let mut writer = reservation.open()?;
    writer.write_all(b"unfinished thumbnail")?;
    writer.flush()?;
    let worker = Worker::start(manager.clone())?;
    for _ in 0..10 {
        worker.handle().scan()?;
        thread::sleep(Duration::from_millis(20));
        assert_eq!(fs::read(&path)?, b"unfinished thumbnail");
        assert_eq!(allocated(&catalog)?, 64);
    }
    drop(writer);
    let recovered = wait_until_reclaimed(&worker, &catalog, &path);
    worker.shutdown()?;
    recovered?;
    drop(manager);
    catalog.shutdown();
    Ok(())
}

#[test]
fn worker_reclaims_unopened_thumbnail_reservation_after_restart() -> anyhow::Result<()> {
    let (root, catalog, manager) = fixture()?;
    let reservation = reserve(&manager)?;
    let path = reservation.path().to_path_buf();
    assert!(!path.exists());
    assert_eq!(allocated(&catalog)?, 64);
    let configuration = manager.configuration().clone();
    drop(reservation);
    drop(manager);
    catalog.shutdown();
    let catalog = RecordingCatalog::open(&root.join("catalog.db"))?;
    let manager = Manager::new(configuration, catalog.handle())?;
    let worker = Worker::start(manager.clone())?;
    let recovered = wait_until_reclaimed(&worker, &catalog, &path);
    worker.shutdown()?;
    recovered?;
    assert!(!path.exists());
    assert_eq!(allocated(&catalog)?, 0);
    drop(manager);
    catalog.shutdown();
    Ok(())
}

#[test]
fn worker_reclaims_created_empty_thumbnail_and_keeps_unrelated_files() -> anyhow::Result<()> {
    let (_root, catalog, manager) = fixture()?;
    let reservation = reserve(&manager)?;
    let path = reservation.path().to_path_buf();
    let unrelated = path.parent().unwrap().join("unrelated.jpg");
    fs::write(&unrelated, b"external image")?;
    drop(reservation.open()?);
    let worker = Worker::start(manager.clone())?;
    let recovered = wait_until_reclaimed(&worker, &catalog, &path);
    worker.shutdown()?;
    recovered?;
    assert_eq!(fs::read(unrelated)?, b"external image");
    drop(manager);
    catalog.shutdown();
    Ok(())
}

#[test]
fn read_only_root_preserves_partial_image_until_writable_restart() -> anyhow::Result<()> {
    let (root, catalog, manager) = fixture()?;
    let reservation = reserve(&manager)?;
    let operation = reservation.operation.clone();
    let path = reservation.path().to_path_buf();
    let mut writer = reservation.open()?;
    writer.write_all(b"partial")?;
    drop(writer);
    let configuration = manager.configuration().clone();
    drop(manager);
    catalog.shutdown();
    let catalog = RecordingCatalog::open(&root.join("catalog.db"))?;
    let mut read_only = configuration.clone();
    read_only.volumes[0].state = super::VolumeState::ReadOnly;
    let manager = Manager::new(read_only, catalog.handle())?;
    assert!(manager.recover_pending_image(&operation).is_err());
    assert_eq!(fs::read(&path)?, b"partial");
    assert_eq!(allocated(&catalog)?, 64);
    drop(manager);
    let manager = Manager::new(configuration, catalog.handle())?;
    assert!(manager.recover_pending_image(&operation)?);
    assert!(!path.exists());
    assert_eq!(allocated(&catalog)?, 0);
    drop(manager);
    catalog.shutdown();
    Ok(())
}
