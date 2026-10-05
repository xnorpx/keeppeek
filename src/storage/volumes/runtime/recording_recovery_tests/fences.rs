use super::*;
use crate::storage::{
    CatalogFragment, CatalogMediaFragment, CatalogRecording, RecordingCatalogHandle,
};

fn insert_alias(
    handle: &RecordingCatalogHandle,
    original: &CatalogMediaFragment,
) -> anyhow::Result<()> {
    let path = if original.path.contains('\\') {
        original.path.replace('\\', "/")
    } else {
        original.path.replace('/', "\\")
    };
    assert_ne!(path, original.path);
    handle.upsert_recording(CatalogRecording {
        id: "legacy-alias".into(),
        stream_id: "alias/main".into(),
        source_id: None,
        logical_stream_id: None,
        started_at_ms: original.recording_started_at_ms,
        ended_at_ms: Some(original.start_ms + i64::try_from(original.duration_ms)?),
        path,
        init_offset: original.init_offset,
        init_len: original.init_len,
        finalized: true,
    })?;
    handle.insert_fragment(CatalogFragment {
        recording_id: "legacy-alias".into(),
        sequence: original.sequence,
        start_ms: original.start_ms,
        duration_ms: original.duration_ms,
        byte_offset: original.byte_offset,
        byte_len: original.byte_len,
        random_access: true,
    })
}

fn begin_recovery(
    catalog: &RecordingCatalog,
    manager: &Manager,
    operation: &str,
) -> anyhow::Result<()> {
    use crate::storage::catalog::locations::recording_recovery::Action;
    let handle = catalog.handle();
    let _writer = handle.claim_volume_move(operation)?;
    let Reply::PendingRecording(Some(pending)) =
        handle.volume_location(Request::RecordingRecovery(Action::Load(operation.into())))?
    else {
        anyhow::bail!("pending recording missing");
    };
    let mut file = manager.inner.writable_root(0)?.open_owned_writable(
        &pending.owned.relative_key,
        pending.owned.file_identity.as_ref().unwrap(),
        pending.owned.materialized_bytes,
        pending.owned.bytes,
    )?;
    let plan = super::super::recording_recovery::recovery_plan(&mut file, &pending)?;
    handle.volume_location(Request::RecordingRecovery(Action::Begin(pending, plan)))?;
    Ok(())
}

#[test]
fn pending_recording_recovery_rejects_new_alias_range_and_cached_snapshot_readers()
-> anyhow::Result<()> {
    let (root, catalog, manager) = tests::fixture(2 * 1_048_576)?;
    let interrupted = interrupted(&manager, &catalog)?;
    drop(interrupted.writer);
    let handle = catalog.handle();
    let original = handle.media_fragments_in_range("camera/main", 0, i64::MAX)?;
    insert_alias(&handle, &original[0])?;
    let (snapshots, lease) = handle.leased_media_fragments_in_range("alias/main", 0, i64::MAX)?;
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].recording_id, "legacy-alias");
    assert!(
        handle
            .reader_leases()
            .conflicts(&interrupted.object.id, &original[0].path)?
    );
    drop(lease);
    let before = fs::read(&interrupted.path)?;
    let allocated_before = allocated(&catalog)?;
    begin_recovery(&catalog, &manager, &interrupted.operation)?;
    assert!(
        handle
            .leased_media_fragments_in_range("alias/main", 0, i64::MAX)
            .is_err()
    );
    assert!(handle.lease_media_fragments(&snapshots).is_err());
    assert!(
        !handle
            .reader_leases()
            .conflicts(&interrupted.object.id, &original[0].path)?
    );
    assert_eq!(fs::read(&interrupted.path)?, before);
    assert_eq!(allocated(&catalog)?, allocated_before);
    assert_eq!(location(&catalog, &interrupted.object)?, None);
    assert!(manager.recover_pending_recording(&interrupted.operation)?);
    let (_, lease) = handle.leased_media_fragments_in_range("alias/main", 0, i64::MAX)?;
    assert!(
        handle
            .reader_leases()
            .conflicts(&interrupted.object.id, &original[0].path)?
    );
    drop(lease);
    assert_eq!(fs::read(&interrupted.path)?, before);
    drop(handle);
    drop(manager);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}
