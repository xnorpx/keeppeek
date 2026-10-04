use super::{
    LegacyPaths,
    inventory::{Action, Evidence},
};
use crate::storage::catalog::{
    self, CatalogFragment, CatalogRecording, RecordingCatalog,
    locations::{Reply, Request},
};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

pub(super) fn fixture(_name: &str, finalized: bool) -> anyhow::Result<(PathBuf, RecordingCatalog)> {
    let (root, handle) = crate::storage::volumes::root::test_root()?;
    drop(handle);
    let final_path = root.join("recording.mp4");
    // ponytail: reuse the original startup fixture and its real fragment byte ranges.
    let (initialization, fragments) = catalog::tests::write_fragmented_recording(&final_path);
    let path = if finalized {
        final_path
    } else {
        root.join("recording.mp4.active")
    };
    let catalog = RecordingCatalog::open(&root.join("catalog.db"))?;
    catalog.handle().upsert_recording(CatalogRecording {
        id: "recording-1".into(),
        stream_id: "front-door/main".into(),
        source_id: Some("192.0.2.10".into()),
        logical_stream_id: Some("main".into()),
        started_at_ms: 1000,
        ended_at_ms: Some(3000),
        path: path.to_string_lossy().into_owned(),
        init_offset: initialization.offset,
        init_len: initialization.size,
        finalized,
    })?;
    for (index, fragment) in fragments.iter().enumerate() {
        catalog.handle().insert_fragment(CatalogFragment {
            recording_id: "recording-1".into(),
            sequence: u64::from(fragment.sequence_number),
            start_ms: 1000 + i64::try_from(index)? * 1000,
            duration_ms: 1000,
            byte_offset: fragment.range.offset,
            byte_len: fragment.range.size,
            random_access: true,
        })?;
    }
    if finalized {
        catalog
            .handle()
            .update_recording_path("recording-1", &path, true)?;
    }
    catalog
        .handle()
        .insert_event(catalog::tests::test_event("event-1", 1500))?;
    assert!(
        catalog
            .handle()
            .resolve_event_keyframe("event-1", "main")?
            .is_none()
    );
    catalog
        .handle()
        .volume_location(Request::RegisterLegacyPaths(Box::new(LegacyPaths {
            active_root: root.clone(),
            archive_root: root.clone(),
            export_root: root.join("exports"),
            thumbnail_root: root.join("images"),
            catalog_path: root.join("catalog.db"),
            export_history_path: root.join("exports/history.json"),
        })))?;
    Ok((root, catalog))
}

#[test]
fn captured_snapshot_still_backfills_healthy_legacy_keyframes() -> anyhow::Result<()> {
    let (root, catalog) = fixture("captured-legacy-keyframe-repair", true)?;
    let before = catalog.handle().stats()?;
    let bytes = std::fs::read(root.join("recording.mp4"))?;
    catalog.shutdown();
    let mut catalog = RecordingCatalog::open(&root.join("catalog.db"))?;
    catalog.wait_for_maintenance();
    let location = catalog
        .handle()
        .resolve_event_keyframe("event-1", "main")?
        .ok_or_else(|| anyhow::anyhow!("healthy legacy keyframe was not backfilled"))?;
    assert_eq!(location.recording_id, "recording-1");
    assert_eq!(location.path, root.join("recording.mp4").to_string_lossy());
    assert_eq!(
        catalog.handle().stats()?.recording_bytes,
        before.recording_bytes
    );
    assert_eq!(catalog.handle().stats()?.recording_files, 1);
    assert_eq!(std::fs::read(root.join("recording.mp4"))?, bytes);
    catalog.shutdown();
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn captured_snapshot_still_recovers_rename_before_catalog_finalization() -> anyhow::Result<()> {
    let (root, catalog) = fixture("captured-legacy-finalization-repair", false)?;
    let bytes = std::fs::read(root.join("recording.mp4"))?;
    assert_eq!(catalog.handle().stats()?.active_files, 1);
    catalog.shutdown();
    let mut catalog = RecordingCatalog::open(&root.join("catalog.db"))?;
    catalog.wait_for_maintenance();
    let location = catalog
        .handle()
        .resolve_event_keyframe("event-1", "main")?
        .ok_or_else(|| anyhow::anyhow!("renamed legacy recording was not recovered"))?;
    assert_eq!(location.path, root.join("recording.mp4").to_string_lossy());
    assert_eq!(catalog.handle().stats()?.finalized_files, 1);
    assert_eq!(catalog.handle().stats()?.active_files, 0);
    assert_eq!(
        catalog.handle().stats()?.recording_bytes,
        u64::try_from(bytes.len())?
    );
    assert_eq!(std::fs::read(root.join("recording.mp4"))?, bytes);
    assert!(!root.join("recording.mp4.active").exists());
    catalog.shutdown();
    std::fs::remove_dir_all(root)?;
    Ok(())
}

fn verified_reference(catalog: &RecordingCatalog, path: &Path) -> anyhow::Result<Reply> {
    let Reply::LegacyReferences(mut page) =
        catalog
            .handle()
            .volume_location(Request::LegacyInventory(Action::Recordings {
                after: None,
                limit: 1,
            }))?
    else {
        anyhow::bail!("inventory page missing")
    };
    assert_eq!(page.len(), 1);
    let identity = catalog::recording_file_identity(path, &std::fs::metadata(path)?)
        .ok_or_else(|| anyhow::anyhow!("fixture identity unavailable"))?;
    catalog
        .handle()
        .volume_location(Request::LegacyInventory(Action::Verify(
            Box::new(page.remove(0)),
            Evidence {
                file_identity: identity.clone(),
                catalog_identity: identity,
                bytes: std::fs::metadata(path)?.len(),
                digest: <sha2::Sha256 as sha2::Digest>::digest(std::fs::read(path)?).into(),
            },
        )))
}

#[test]
fn captured_snapshot_preserves_known_evidence_when_legacy_file_changes() -> anyhow::Result<()> {
    for replaced in [false, true] {
        let (root, catalog) = fixture("captured-legacy-changed-evidence", true)?;
        let path = root.join("recording.mp4");
        let verified = verified_reference(&catalog, &path)?;
        let Reply::LegacyReference(Some(reference)) = &verified else {
            anyhow::bail!("verified inventory missing")
        };
        let before = catalog.handle().stats()?;
        catalog.shutdown();
        if replaced {
            let previous = root.join("retained-original.mp4");
            std::fs::rename(&path, &previous)?;
            std::fs::copy(previous, &path)?;
            let identity = catalog::recording_file_identity(&path, &std::fs::metadata(&path)?);
            assert_ne!(
                identity.as_ref(),
                Some(&reference.evidence.as_ref().unwrap().catalog_identity)
            );
        } else {
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)?
                .write_all(b"changed tail")?;
        }
        let changed_bytes = std::fs::read(&path)?;
        let mut catalog = RecordingCatalog::open(&root.join("catalog.db"))?;
        catalog.wait_for_maintenance();
        assert_eq!(
            catalog.handle().stats()?.recording_bytes,
            before.recording_bytes
        );
        assert_eq!(catalog.handle().stats()?.recording_files, 1);
        assert_eq!(
            catalog
                .handle()
                .volume_location(Request::LegacyInventory(Action::Lookup(
                    reference.object.clone()
                ),))?,
            verified
        );
        assert!(
            catalog
                .handle()
                .resolve_event_keyframe("event-1", "main")?
                .is_none()
        );
        assert_eq!(std::fs::read(&path)?, changed_bytes);
        catalog.shutdown();
        std::fs::remove_dir_all(root)?;
    }
    Ok(())
}
