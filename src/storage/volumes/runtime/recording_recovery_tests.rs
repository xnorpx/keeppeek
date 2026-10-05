use super::{Kind, Manager, Object, Reply, Request, VolumeRole, tests};
mod abandonment;
mod fences;
use crate::storage::{
    MediaFrame, RecordingCatalog, RecordingFrame, RecordingStreamIdentity, VideoCodec, VideoFrame,
    catalog::locations::Location, medium_term::MediumTermWriter,
};
use bytes::Bytes;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

struct Interrupted {
    object: Object,
    operation: String,
    path: PathBuf,
    writer: MediumTermWriter,
}

fn interrupted(manager: &Manager, catalog: &RecordingCatalog) -> anyhow::Result<Interrupted> {
    let object = Object {
        kind: Kind::Recording,
        id: uuid::Uuid::new_v4().to_string(),
    };
    let reservation = manager
        .reserve(VolumeRole::Active, "camera", &[], object.clone(), 1)?
        .unwrap();
    let operation = reservation.operation.clone();
    let path = reservation.path().to_path_buf();
    let start = Instant::now();
    // ponytail: a one-byte buffer exposes completed fragments without a test-only flush API.
    let mut writer = MediumTermWriter::create_with_reservation(
        reservation,
        object.id.clone(),
        RecordingStreamIdentity::legacy("camera/main"),
        start,
        1,
        catalog.handle(),
    )?;
    for seconds in 0..3 {
        writer.append_one(frame(start, seconds))?;
    }
    assert_eq!(catalog.handle().stats()?.finalized_files, 0);
    assert_eq!(
        catalog
            .handle()
            .fragments_in_range("camera/main", 0, i64::MAX)?
            .len(),
        2
    );
    Ok(Interrupted {
        object,
        operation,
        path,
        writer,
    })
}

fn frame(start: Instant, seconds: u64) -> RecordingFrame {
    let timestamp = Duration::from_secs(seconds);
    RecordingFrame {
        received_at: start + timestamp,
        timestamp: Some(timestamp),
        frame: MediaFrame::Video(VideoFrame {
            codec: VideoCodec::H264,
            is_keyframe: true,
            width: 320,
            height: 240,
            data: Bytes::from_static(&[
                0, 0, 0, 8, 0x67, 0x42, 0x00, 0x1f, 0xe5, 0x88, 0x68, 0x40, 0, 0, 0, 4, 0x68, 0xce,
                0x3c, 0x80, 0, 0, 0, 1, 0x65,
            ]),
        }),
    }
}

fn allocated(catalog: &RecordingCatalog) -> anyhow::Result<u64> {
    let Reply::Usage(usage) = catalog.handle().volume_location(Request::Usage)? else {
        anyhow::bail!("volume usage reply missing");
    };
    Ok(usage.iter().map(|volume| volume.allocated_bytes).sum())
}

fn location(catalog: &RecordingCatalog, object: &Object) -> anyhow::Result<Option<Location>> {
    let Reply::Location(location) = catalog
        .handle()
        .volume_location(Request::Lookup(object.clone()))?
    else {
        anyhow::bail!("recording location reply missing");
    };
    Ok(location)
}

#[test]
fn live_recording_is_untouched_then_dropped_indexed_fragments_are_recovered() -> anyhow::Result<()>
{
    let (root, catalog, manager) = tests::fixture(2 * 1_048_576)?;
    let interrupted = interrupted(&manager, &catalog)?;
    let before = fs::read(&interrupted.path)?;
    let allocated_before = allocated(&catalog)?;
    let _attempt = manager.recover_pending_recording(&interrupted.operation);
    assert_eq!(fs::read(&interrupted.path)?, before);
    assert_eq!(allocated(&catalog)?, allocated_before);
    assert_eq!(location(&catalog, &interrupted.object)?, None);
    assert_eq!(catalog.handle().stats()?.finalized_files, 0);
    drop(interrupted.writer);
    assert!(manager.recover_pending_recording(&interrupted.operation)?);
    let published = location(&catalog, &interrupted.object)?.unwrap();
    assert_eq!(published.bytes, u64::try_from(before.len())?);
    assert_eq!(allocated(&catalog)?, published.bytes);
    assert_eq!(catalog.handle().stats()?.finalized_files, 1);
    assert_eq!(fs::read(&interrupted.path)?, before);
    let parsed = mp4::read_mp4(fs::File::open(&interrupted.path)?)?;
    assert_eq!(parsed.tracks().len(), 1);
    assert_eq!(parsed.tracks().values().next().unwrap().sample_count(), 2);
    drop(parsed);
    drop(manager);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn interrupted_recording_recovers_after_restart_and_retries_without_changing_location()
-> anyhow::Result<()> {
    let (root, catalog, manager) = tests::fixture(2 * 1_048_576)?;
    let interrupted = interrupted(&manager, &catalog)?;
    let configuration = manager.configuration().clone();
    drop(interrupted.writer);
    let before = fs::read(&interrupted.path)?;
    drop(manager);
    catalog.shutdown();
    let catalog = RecordingCatalog::open(&root.join("catalog.db"))?;
    let manager = Manager::new(configuration.clone(), catalog.handle())?;
    assert_eq!(location(&catalog, &interrupted.object)?, None);
    assert!(manager.recover_pending_recording(&interrupted.operation)?);
    let published = location(&catalog, &interrupted.object)?.unwrap();
    let fragments = catalog
        .handle()
        .fragments_in_range("camera/main", 0, i64::MAX)?;
    assert_eq!(fragments.len(), 2);
    drop(manager);
    catalog.shutdown();
    let catalog = RecordingCatalog::open(&root.join("catalog.db"))?;
    let manager = Manager::new(configuration, catalog.handle())?;
    manager.recover_pending_recording(&interrupted.operation)?;
    assert_eq!(
        location(&catalog, &interrupted.object)?,
        Some(published.clone())
    );
    assert_eq!(catalog.handle().stats()?.finalized_files, 1);
    assert_eq!(allocated(&catalog)?, published.bytes);
    assert_eq!(
        catalog
            .handle()
            .fragments_in_range("camera/main", 0, i64::MAX)?,
        fragments
    );
    assert_eq!(fs::read(&interrupted.path)?, before);
    drop(manager);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn malformed_interrupted_recording_keeps_its_file_and_capacity_ownership() -> anyhow::Result<()> {
    let (root, catalog, manager) = tests::fixture(2 * 1_048_576)?;
    let interrupted = interrupted(&manager, &catalog)?;
    drop(interrupted.writer);
    let malformed = vec![0_u8; usize::try_from(fs::metadata(&interrupted.path)?.len())?];
    fs::write(&interrupted.path, &malformed)?;
    let allocated_before = allocated(&catalog)?;
    let fragments = catalog
        .handle()
        .fragments_in_range("camera/main", 0, i64::MAX)?;
    assert!(
        manager
            .recover_pending_recording(&interrupted.operation)
            .is_err()
    );
    assert_eq!(location(&catalog, &interrupted.object)?, None);
    assert_eq!(allocated(&catalog)?, allocated_before);
    assert_eq!(catalog.handle().stats()?.finalized_files, 0);
    assert_eq!(
        catalog
            .handle()
            .fragments_in_range("camera/main", 0, i64::MAX)?,
        fragments
    );
    assert_eq!(fs::read(&interrupted.path)?, malformed);
    drop(manager);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn replacement_interrupted_recording_is_not_adopted_or_removed() -> anyhow::Result<()> {
    let (root, catalog, manager) = tests::fixture(2 * 1_048_576)?;
    let interrupted = interrupted(&manager, &catalog)?;
    drop(interrupted.writer);
    let original = fs::read(&interrupted.path)?;
    let preserved = root.join("original.mp4");
    fs::rename(&interrupted.path, &preserved)?;
    fs::write(&interrupted.path, &original)?;
    let allocated_before = allocated(&catalog)?;
    assert!(
        manager
            .recover_pending_recording(&interrupted.operation)
            .is_err()
    );
    assert_eq!(location(&catalog, &interrupted.object)?, None);
    assert_eq!(allocated(&catalog)?, allocated_before);
    assert_eq!(catalog.handle().stats()?.finalized_files, 0);
    assert_eq!(fs::read(&interrupted.path)?, original);
    assert_eq!(fs::read(&preserved)?, original);
    drop(manager);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn recovery_discards_only_the_torn_unindexed_tail_after_complete_fragments() -> anyhow::Result<()> {
    let (root, catalog, manager) = tests::fixture(2 * 1_048_576)?;
    let interrupted = interrupted(&manager, &catalog)?;
    drop(interrupted.writer);
    let prefix = fs::read(&interrupted.path)?;
    let fragments = catalog
        .handle()
        .fragments_in_range("camera/main", 0, i64::MAX)?;
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&interrupted.path)?;
    file.write_all(b"\0\0\0\x40moof\0\0\0\x10mfhd")?;
    file.sync_all()?;
    drop(file);
    assert!(fs::metadata(&interrupted.path)?.len() > u64::try_from(prefix.len())?);
    assert!(manager.recover_pending_recording(&interrupted.operation)?);
    let published = location(&catalog, &interrupted.object)?.unwrap();
    assert_eq!(published.bytes, u64::try_from(prefix.len())?);
    assert_eq!(allocated(&catalog)?, published.bytes);
    assert_eq!(fs::read(&interrupted.path)?, prefix);
    assert_eq!(
        catalog
            .handle()
            .fragments_in_range("camera/main", 0, i64::MAX)?,
        fragments
    );
    let parsed = mp4::read_mp4(fs::File::open(&interrupted.path)?)?;
    assert_eq!(parsed.tracks().values().next().unwrap().sample_count(), 2);
    drop(parsed);
    drop(manager);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn recovery_removes_indexed_fragment_and_keyframe_when_its_bytes_were_not_completed()
-> anyhow::Result<()> {
    use crate::storage::CatalogEventKeyframeLink;
    let (root, catalog, manager) = tests::fixture(2 * 1_048_576)?;
    let interrupted = interrupted(&manager, &catalog)?;
    drop(interrupted.writer);
    let handle = catalog.handle();
    let fragments = handle.fragments_in_range("camera/main", 0, i64::MAX)?;
    let prefix_bytes = fragments[0].byte_offset + fragments[0].byte_len;
    let prefix = fs::read(&interrupted.path)?[..usize::try_from(prefix_bytes)?].to_vec();
    let link = CatalogEventKeyframeLink {
        event_id: "recovery-event".into(),
        stream_id: "main".into(),
        recording_id: interrupted.object.id.clone(),
        fragment_sequence: fragments[1].sequence,
    };
    handle.insert_event(crate::storage::catalog::tests::test_event(
        "recovery-event",
        fragments[1].start_ms,
    ))?;
    handle.link_event_keyframe(link.clone())?;
    assert!(
        handle
            .resolve_event_keyframe("recovery-event", "main")?
            .is_some()
    );
    let file = fs::OpenOptions::new().write(true).open(&interrupted.path)?;
    file.set_len(prefix_bytes + 8)?;
    file.sync_all()?;
    drop(file);
    assert!(manager.recover_pending_recording(&interrupted.operation)?);
    assert_eq!(
        location(&catalog, &interrupted.object)?.unwrap().bytes,
        prefix_bytes
    );
    assert_eq!(allocated(&catalog)?, prefix_bytes);
    assert_eq!(fs::read(&interrupted.path)?, prefix);
    assert_eq!(
        handle.fragments_in_range("camera/main", 0, i64::MAX)?,
        fragments[..1]
    );
    assert_eq!(handle.stats()?.fragments, 1);
    assert_eq!(handle.stats()?.finalized_files, 1);
    assert!(
        handle
            .resolve_event_keyframe("recovery-event", "main")?
            .is_none()
    );
    assert!(handle.link_event_keyframe(link).is_err());
    assert!(handle.event_by_id("recovery-event")?.is_some());
    let parsed = mp4::read_mp4(fs::File::open(&interrupted.path)?)?;
    assert_eq!(parsed.tracks().values().next().unwrap().sample_count(), 1);
    drop(parsed);
    drop(handle);
    drop(manager);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn reader_lease_defers_recording_recovery_without_truncation_or_publication() -> anyhow::Result<()>
{
    let (root, catalog, manager) = tests::fixture(2 * 1_048_576)?;
    let interrupted = interrupted(&manager, &catalog)?;
    drop(interrupted.writer);
    let prefix = fs::read(&interrupted.path)?;
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&interrupted.path)?;
    file.write_all(b"\0\0\0\x40moof")?;
    file.sync_all()?;
    drop(file);
    let before = fs::read(&interrupted.path)?;
    let allocated_before = allocated(&catalog)?;
    let (fragments, lease) =
        catalog
            .handle()
            .leased_media_fragments_in_range("camera/main", 0, i64::MAX)?;
    assert_eq!(fragments.len(), 2);
    let _attempt = manager.recover_pending_recording(&interrupted.operation);
    assert_eq!(fs::read(&interrupted.path)?, before);
    assert_eq!(allocated(&catalog)?, allocated_before);
    assert_eq!(location(&catalog, &interrupted.object)?, None);
    assert_eq!(catalog.handle().stats()?.finalized_files, 0);
    drop(lease);
    assert!(manager.recover_pending_recording(&interrupted.operation)?);
    assert_eq!(fs::read(&interrupted.path)?, prefix);
    assert_eq!(catalog.handle().stats()?.finalized_files, 1);
    assert_eq!(
        location(&catalog, &interrupted.object)?.unwrap().bytes,
        u64::try_from(prefix.len())?
    );
    drop(manager);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

fn mutate_stopped_catalog(root: &Path, sql: &str) -> anyhow::Result<()> {
    let mut lease = crate::storage::catalog::authority::Lease::acquire(&root.join("catalog.db"))?;
    let connection = lease.connect()?;
    lease.verify(&connection)?;
    pollster::block_on(connection.execute_batch(sql))?;
    Ok(())
}

#[test]
fn recovery_refuses_changed_catalog_path_sequence_or_timing_before_truncation() -> anyhow::Result<()>
{
    for mutation in [
        "UPDATE recording_files SET path=path||'.other'",
        "UPDATE recording_fragments SET start_ms=start_ms+123 WHERE sequence=1",
        "PRAGMA foreign_keys=OFF; BEGIN IMMEDIATE;
         UPDATE recording_fragments SET sequence=sequence+10;
         UPDATE recording_keyframes SET fragment_sequence=fragment_sequence+10; COMMIT;",
    ] {
        let (root, catalog, manager) = tests::fixture(2 * 1_048_576)?;
        let interrupted = interrupted(&manager, &catalog)?;
        drop(interrupted.writer);
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&interrupted.path)?;
        file.write_all(b"\0\0\0\x40moof")?;
        file.sync_all()?;
        drop(file);
        let before = fs::read(&interrupted.path)?;
        let allocated_before = allocated(&catalog)?;
        let configuration = manager.configuration().clone();
        drop(manager);
        catalog.shutdown();
        mutate_stopped_catalog(&root, mutation)?;
        let catalog = RecordingCatalog::open(&root.join("catalog.db"))?;
        let manager = Manager::new(configuration, catalog.handle())?;
        assert!(
            manager
                .recover_pending_recording(&interrupted.operation)
                .is_err()
        );
        assert_eq!(fs::read(&interrupted.path)?, before);
        assert_eq!(allocated(&catalog)?, allocated_before);
        assert_eq!(location(&catalog, &interrupted.object)?, None);
        assert_eq!(catalog.handle().stats()?.fragments, 2);
        assert_eq!(catalog.handle().stats()?.finalized_files, 0);
        drop(manager);
        catalog.shutdown();
        fs::remove_dir_all(root)?;
    }
    Ok(())
}

#[test]
fn missing_catalog_keyframe_preserves_corrupt_metadata_and_all_file_bytes() -> anyhow::Result<()> {
    let (root, catalog, manager) = tests::fixture(2 * 1_048_576)?;
    let interrupted = interrupted(&manager, &catalog)?;
    drop(interrupted.writer);
    let fragments = catalog
        .handle()
        .fragments_in_range("camera/main", 0, i64::MAX)?;
    let before = fs::read(&interrupted.path)?;
    let allocated_before = allocated(&catalog)?;
    let configuration = manager.configuration().clone();
    drop(manager);
    catalog.shutdown();
    mutate_stopped_catalog(
        &root,
        "DELETE FROM recording_keyframes WHERE fragment_sequence=2",
    )?;
    let catalog = RecordingCatalog::open(&root.join("catalog.db"))?;
    let manager = Manager::new(configuration, catalog.handle())?;
    assert!(
        manager
            .recover_pending_recording(&interrupted.operation)
            .is_err()
    );
    assert_eq!(location(&catalog, &interrupted.object)?, None);
    assert_eq!(allocated(&catalog)?, allocated_before);
    assert_eq!(catalog.handle().stats()?.fragments, 2);
    assert_eq!(catalog.handle().stats()?.finalized_files, 0);
    assert_eq!(
        catalog
            .handle()
            .fragments_in_range("camera/main", 0, i64::MAX)?,
        fragments
    );
    assert_eq!(fs::read(&interrupted.path)?, before);
    drop(manager);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

fn persist_then_truncate(
    catalog: &RecordingCatalog,
    manager: &Manager,
    operation: &str,
) -> anyhow::Result<crate::storage::catalog::locations::recording_recovery::Plan> {
    use crate::storage::catalog::locations::recording_recovery::Action;
    let handle = catalog.handle();
    let _writer = handle.claim_volume_move(operation)?;
    let Reply::PendingRecording(Some(pending)) =
        handle.volume_location(Request::RecordingRecovery(Action::Load(operation.into())))?
    else {
        anyhow::bail!("pending recording missing")
    };
    let mut file = manager.inner.writable_root(0)?.open_owned_writable(
        &pending.owned.relative_key,
        pending.owned.file_identity.as_ref().unwrap(),
        pending.owned.materialized_bytes,
        pending.owned.bytes,
    )?;
    let plan = super::recording_recovery::recovery_plan(&mut file, &pending)?;
    handle.volume_location(Request::RecordingRecovery(Action::Begin(
        pending,
        plan.clone(),
    )))?;
    assert!(
        handle
            .leased_media_fragments_in_range("camera/main", 0, i64::MAX)
            .is_err()
    );
    let mut fragment = handle.fragments_in_range("camera/main", 0, i64::MAX)?[0].clone();
    fragment.sequence = 99;
    fragment.start_ms += 100_000;
    assert!(handle.insert_fragment(fragment).is_err());
    assert_eq!(handle.stats()?.fragments, 2);
    file.retain_verified_prefix(
        plan.evidence.bytes,
        plan.original_bytes,
        plan.evidence.digest,
    )?;
    Ok(plan)
}

#[test]
fn committed_recovery_plan_resumes_after_crash_following_truncation() -> anyhow::Result<()> {
    use crate::storage::catalog::locations::recording_recovery::Action;
    let (root, catalog, manager) = tests::fixture(2 * 1_048_576)?;
    let interrupted = interrupted(&manager, &catalog)?;
    drop(interrupted.writer);
    let fragments = catalog
        .handle()
        .fragments_in_range("camera/main", 0, i64::MAX)?;
    let endpoint = fragments[0].byte_offset + fragments[0].byte_len;
    let prefix = fs::read(&interrupted.path)?[..usize::try_from(endpoint)?].to_vec();
    let file = fs::OpenOptions::new().write(true).open(&interrupted.path)?;
    file.set_len(endpoint + 8)?;
    file.sync_all()?;
    drop(file);
    let allocated_before = allocated(&catalog)?;
    let plan = persist_then_truncate(&catalog, &manager, &interrupted.operation)?;
    assert_eq!(plan.last_sequence, fragments[0].sequence);
    assert_eq!(fs::read(&interrupted.path)?, prefix);
    assert_eq!(allocated(&catalog)?, allocated_before);
    assert_eq!(location(&catalog, &interrupted.object)?, None);
    assert_eq!(catalog.handle().stats()?.finalized_files, 0);
    let configuration = manager.configuration().clone();
    drop(manager);
    catalog.shutdown();
    let catalog = RecordingCatalog::open(&root.join("catalog.db"))?;
    let manager = Manager::new(configuration, catalog.handle())?;
    let Reply::PendingRecording(Some(pending)) =
        catalog
            .handle()
            .volume_location(Request::RecordingRecovery(Action::Load(
                interrupted.operation.clone(),
            )))?
    else {
        anyhow::bail!("recovery plan did not survive restart")
    };
    assert_eq!(pending.plan, Some(plan.clone()));
    assert_eq!(allocated(&catalog)?, allocated_before);
    assert!(
        catalog
            .handle()
            .leased_media_fragments_in_range("camera/main", 0, i64::MAX)
            .is_err()
    );
    assert!(manager.recover_pending_recording(&interrupted.operation)?);
    let published = location(&catalog, &interrupted.object)?.unwrap();
    assert_eq!(published.bytes, plan.evidence.bytes);
    assert_eq!(published.file_identity, plan.evidence.file_identity);
    assert_eq!(published.digest, plan.evidence.digest);
    assert_eq!(allocated(&catalog)?, endpoint);
    assert_eq!(catalog.handle().stats()?.finalized_files, 1);
    let (retained, lease) =
        catalog
            .handle()
            .leased_media_fragments_in_range("camera/main", 0, i64::MAX)?;
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].sequence, fragments[0].sequence);
    drop(lease);
    manager.recover_pending_recording(&interrupted.operation)?;
    assert_eq!(location(&catalog, &interrupted.object)?, Some(published));
    assert_eq!(fs::read(&interrupted.path)?, prefix);
    drop(manager);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn unopened_recording_reservation_is_released_after_restart_without_creating_a_file()
-> anyhow::Result<()> {
    let (root, catalog, manager) = tests::fixture(2 * 1_048_576)?;
    let object = Object {
        kind: Kind::Recording,
        id: uuid::Uuid::new_v4().to_string(),
    };
    let reservation = manager
        .reserve(VolumeRole::Active, "camera", &[], object.clone(), 64)?
        .unwrap();
    let operation = reservation.operation.clone();
    let path = reservation.path().to_path_buf();
    let _attempt = manager.recover_pending_recording(&operation);
    assert!(!path.exists());
    assert_eq!(allocated(&catalog)?, 64);
    let configuration = manager.configuration().clone();
    drop(reservation);
    drop(manager);
    catalog.shutdown();
    let catalog = RecordingCatalog::open(&root.join("catalog.db"))?;
    let manager = Manager::new(configuration, catalog.handle())?;
    assert!(manager.recover_pending_recording(&operation)?);
    assert!(!path.exists());
    assert_eq!(allocated(&catalog)?, 0);
    assert_eq!(location(&catalog, &object)?, None);
    assert_eq!(catalog.handle().stats()?.recording_files, 0);
    manager.recover_pending_recording(&operation)?;
    assert_eq!(allocated(&catalog)?, 0);
    assert!(!path.exists());
    drop(manager);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}
