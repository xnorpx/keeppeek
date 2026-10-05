use super::{encode_jpeg, fixture, image_event, store};
use crate::storage::catalog::{
    CatalogEventKeyframeLink, CatalogFragment, CatalogKeyframe, CatalogRecording,
    RecordingCatalogHandle,
    locations::{Reply, Request},
};
use image::DynamicImage;
use std::{fs, path::Path};

fn recording(handle: &RecordingCatalogHandle, root: &Path, id: &str) -> anyhow::Result<()> {
    let path = root.join(format!("{id}.mp4"));
    fs::write(&path, [0_u8; 32])?;
    handle.upsert_recording(CatalogRecording {
        id: id.into(),
        stream_id: "front-door/sub".into(),
        source_id: Some("front-door".into()),
        logical_stream_id: Some("sub".into()),
        started_at_ms: 2_000,
        ended_at_ms: Some(3_000),
        path: path.to_string_lossy().into_owned(),
        init_offset: 0,
        init_len: 8,
        finalized: true,
    })?;
    // ponytail: public explicit links isolate admission ordering from automatic time matching.
    handle.insert_fragment_with_keyframe(
        CatalogFragment {
            recording_id: id.into(),
            sequence: 1,
            start_ms: 2_000,
            duration_ms: 1_000,
            byte_offset: 8,
            byte_len: 24,
            random_access: true,
        },
        CatalogKeyframe {
            recording_id: id.into(),
            fragment_sequence: 1,
            byte_offset: 8,
            byte_len: 24,
        },
    )
}

fn link(recording_id: &str) -> CatalogEventKeyframeLink {
    CatalogEventKeyframeLink {
        event_id: "event".into(),
        stream_id: "sub".into(),
        recording_id: recording_id.into(),
        fragment_sequence: 1,
    }
}

#[test]
fn image_pressure_rejects_late_protected_association_and_preserves_existing_link()
-> anyhow::Result<()> {
    let (root, catalog, config) = fixture(1024 * 1024, true)?;
    let events = store(&catalog, &config)?;
    let jpeg = encode_jpeg(&DynamicImage::new_rgb8(24, 16))?;
    events.commit_published_image("publication", image_event("event", &jpeg), &jpeg)?;
    let path = events.thumbnail_path("front-door", "event")?.unwrap();
    let handle = catalog.handle();
    recording(&handle, &root, "protected")?;
    recording(&handle, &root, "ordinary")?;
    handle.set_recording_protected("protected", true)?;
    assert!(handle.resolve_event_keyframe("event", "sub")?.is_none());
    assert_eq!(
        handle.volume_location(Request::ImagePressure("images".into()))?,
        Reply::ImagePressure(true)
    );
    assert!(handle.link_event_keyframe(link("protected")).is_err());
    assert!(handle.resolve_event_keyframe("event", "sub")?.is_none());
    handle.link_event_keyframe(link("ordinary"))?;
    assert!(handle.link_event_keyframe(link("protected")).is_err());
    assert_eq!(
        handle
            .resolve_event_keyframe("event", "sub")?
            .unwrap()
            .recording_id,
        "ordinary"
    );
    assert_eq!(handle.stats()?.protected_files, 1);
    assert_eq!(fs::read(&path)?, jpeg);
    assert_eq!(fs::read(root.join("protected.mp4"))?, [0_u8; 32]);
    assert_eq!(fs::read(root.join("ordinary.mp4"))?, [0_u8; 32]);
    drop(handle);
    drop(events);
    drop(config);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn image_pressure_skips_an_image_linked_to_a_protected_recording() -> anyhow::Result<()> {
    let (root, catalog, config) = fixture(1024 * 1024, true)?;
    let events = store(&catalog, &config)?;
    let jpeg = encode_jpeg(&DynamicImage::new_rgb8(24, 16))?;
    events.commit_published_image("publication", image_event("event", &jpeg), &jpeg)?;
    let path = events.thumbnail_path("front-door", "event")?.unwrap();
    let handle = catalog.handle();
    recording(&handle, &root, "protected")?;
    handle.link_event_keyframe(link("protected"))?;
    handle.set_recording_protected("protected", true)?;
    assert_eq!(
        handle.volume_location(Request::ImagePressure("images".into()))?,
        Reply::ImagePressure(false)
    );
    assert_eq!(handle.stats()?.protected_files, 1);
    assert_eq!(fs::read(&path)?, jpeg);
    let event = events.event_by_id("event")?.unwrap();
    assert!(events.leased_attachment_path(&event, "snapshot")?.is_some());
    assert_eq!(fs::read(root.join("protected.mp4"))?, [0_u8; 32]);
    drop(handle);
    drop(events);
    drop(config);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn admitted_image_pressure_rejects_a_linked_recording_protection_upgrade() -> anyhow::Result<()> {
    let (root, catalog, config) = fixture(1024 * 1024, true)?;
    let events = store(&catalog, &config)?;
    let jpeg = encode_jpeg(&DynamicImage::new_rgb8(24, 16))?;
    events.commit_published_image("publication", image_event("event", &jpeg), &jpeg)?;
    let path = events.thumbnail_path("front-door", "event")?.unwrap();
    let handle = catalog.handle();
    recording(&handle, &root, "ordinary")?;
    handle.link_event_keyframe(link("ordinary"))?;
    assert_eq!(
        handle.volume_location(Request::ImagePressure("images".into()))?,
        Reply::ImagePressure(true)
    );
    assert!(handle.set_recording_protected("ordinary", true).is_err());
    assert_eq!(handle.stats()?.protected_files, 0);
    assert_eq!(
        handle
            .resolve_event_keyframe("event", "sub")?
            .unwrap()
            .recording_id,
        "ordinary"
    );
    assert_eq!(fs::read(&path)?, jpeg);
    assert_eq!(fs::read(root.join("ordinary.mp4"))?, [0_u8; 32]);
    drop(handle);
    drop(events);
    drop(config);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}
