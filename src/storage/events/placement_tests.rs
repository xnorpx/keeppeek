use super::{EventStore, PublishedImageCommit, encode_jpeg, tests::image_event};
use crate::storage::{
    catalog::RecordingCatalog,
    engine::StorageConfig,
    volumes::{
        PlacementRule, PlacementStrategy, Volume, VolumeConfiguration, VolumeId, VolumeRole,
        VolumeState, runtime::Manager,
    },
};
use image::DynamicImage;
use std::{fs, path::Path, path::PathBuf, sync::Arc};

mod pressure;

#[test]
fn committed_image_retry_survives_event_close_and_rejects_changed_evidence() -> anyhow::Result<()> {
    use crate::storage::catalog::locations::{Kind, Object, Reply, Request, images};
    use std::io::Write;
    let (root, catalog, config) = fixture(1024 * 1024, true)?;
    let jpeg = encode_jpeg(&DynamicImage::new_rgb8(24, 16))?;
    let object_id = uuid::Uuid::new_v4().to_string();
    let mut writer = config
        .volume_runtime
        .as_ref()
        .unwrap()
        .reserve(
            VolumeRole::Thumbnail,
            "front-door",
            &[],
            Object {
                kind: Kind::Thumbnail,
                id: object_id.clone(),
            },
            jpeg.len() as u64,
        )?
        .unwrap()
        .open()?;
    writer.write_all(&jpeg)?;
    let evidence = writer.seal_image()?;
    let mut event = image_event("retry-event", &jpeg);
    event.thumbnail_filename = Some(format!("{object_id}.jpg"));
    let mut commit = images::Commit {
        event,
        publication: None,
        images: vec![images::Image {
            attachment_id: "snapshot".into(),
            object_id,
            evidence,
        }],
    };
    let handle = catalog.handle();
    assert_eq!(
        handle.volume_location(Request::CommitImages(Box::new(commit.clone())))?,
        Reply::Bound
    );
    handle.close_event("retry-event", 2000)?;
    let closed = handle.event_by_id("retry-event")?.unwrap();
    assert_eq!(
        handle.volume_location(Request::CommitImages(Box::new(commit.clone())))?,
        Reply::Bound
    );
    assert_eq!(handle.event_by_id("retry-event")?.unwrap(), closed);
    commit.images[0].evidence.digest[0] ^= 1;
    assert!(
        handle
            .volume_location(Request::CommitImages(Box::new(commit)))
            .is_err()
    );
    drop(writer);
    drop(config);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

fn protected_directory(path: &Path) -> anyhow::Result<()> {
    fs::create_dir(path)?;
    #[cfg(windows)]
    anyhow::ensure!(
        std::process::Command::new("powershell.exe")
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"])
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/.github/scripts/protect-test-directory.ps1"
            ))
            .arg("-Directory")
            .arg(path)
            .status()?
            .success(),
        "cannot protect thumbnail fixture"
    );
    Ok(())
}

fn fixture(limit: u64, online: bool) -> anyhow::Result<(PathBuf, RecordingCatalog, StorageConfig)> {
    let base = std::env::temp_dir();
    #[cfg(unix)]
    let base = fs::canonicalize(base)?;
    let root = base.join(format!(
        "keeppeek-thumbnail-placement-{}",
        uuid::Uuid::new_v4()
    ));
    protected_directory(&root)?;
    let destination = root.join("named");
    if online {
        protected_directory(&destination)?;
    }
    let id = VolumeId::parse("images")?;
    let configuration = VolumeConfiguration {
        volumes: vec![Volume {
            id: id.clone(),
            root: destination,
            roles: vec![VolumeRole::Thumbnail],
            state: VolumeState::Enabled,
            priority: 0,
            capacity_bytes: Some(limit),
            minimum_free_bytes: 0,
            warning_free_bytes: 0,
            critical_free_bytes: 0,
            sources: vec![],
            groups: vec![],
        }],
        placement: vec![PlacementRule {
            role: VolumeRole::Thumbnail,
            source: Some("front-door".into()),
            group: None,
            candidates: vec![id],
            strategy: PlacementStrategy::Priority,
            allow_fallback: false,
        }],
    };
    let catalog = RecordingCatalog::open(&root.join("catalog.db"))?;
    let config = StorageConfig {
        volume_runtime: Some(Arc::new(Manager::new(
            configuration.clone(),
            catalog.handle(),
        )?)),
        named_volumes: Some(configuration),
        event_thumbnail_path: root.join("legacy"),
        ..StorageConfig::default()
    };
    Ok((root, catalog, config))
}

fn store(catalog: &RecordingCatalog, config: &StorageConfig) -> anyhow::Result<EventStore> {
    Ok(
        EventStore::new(catalog.handle(), &config.event_thumbnail_path, 0)?
            .with_volume_storage(config),
    )
}

#[test]
fn named_image_pressure_waits_for_reader_and_reclaims_only_owned_image() -> anyhow::Result<()> {
    use crate::storage::volumes::runtime::worker::Worker;
    let jpeg = encode_jpeg(&DynamicImage::new_rgb8(24, 16))?;
    let (_root, catalog, config) = fixture(2 * jpeg.len() as u64 + 10, true)?;
    let events = store(&catalog, &config)?;
    events.commit_published_image("pressure-first", image_event("old-image", &jpeg), &jpeg)?;
    events.commit_published_image("pressure-other", image_event("other-image", &jpeg), &jpeg)?;
    let other = events.thumbnail_path("front-door", "other-image")?.unwrap();
    let old = events.event_by_id("old-image")?.unwrap();
    let (path, lease) = events.leased_attachment_path(&old, "snapshot")?.unwrap();
    let unrelated = config.event_thumbnail_path.join(path.file_name().unwrap());
    fs::write(&unrelated, b"unrelated legacy file")?;
    let worker = Worker::start(config.volume_runtime.as_ref().unwrap().as_ref().clone())?;
    assert!(
        events
            .commit_published_image("pressure-next", image_event("next-image", &jpeg), &jpeg)
            .is_err()
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    for _ in 0..3 {
        assert!(
            events
                .commit_published_image("pressure-next", image_event("next-image", &jpeg), &jpeg)
                .is_err()
        );
    }
    assert_eq!(fs::read(&path)?, jpeg);
    assert!(other.exists());
    drop(lease);
    for _ in 0..100 {
        worker.handle().scan()?;
        if !path.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    worker.shutdown()?;
    assert!(
        !path.exists(),
        "volume pressure did not retire its oldest image"
    );
    assert!(events.thumbnail_path("front-door", "old-image").is_err());
    assert_eq!(fs::read(unrelated)?, b"unrelated legacy file");
    assert!(
        other.exists(),
        "repeated pressure must reuse its pending retirement"
    );
    assert_eq!(events.event_by_id("old-image")?.unwrap(), old);
    events.commit_published_image("pressure-next", image_event("next-image", &jpeg), &jpeg)?;
    assert!(events.thumbnail_path("front-door", "next-image")?.is_some());
    drop(events);
    drop(config);
    catalog.shutdown();
    Ok(())
}

#[test]
fn snapshot_thumbnail_uses_the_matched_named_volume() -> anyhow::Result<()> {
    let (root, catalog, config) = fixture(1024 * 1024, true)?;
    let events = store(&catalog, &config)?;
    let jpeg = encode_jpeg(&DynamicImage::new_rgb8(640, 480))?;
    events.insert(image_event("snapshot-event", &jpeg))?;
    events.save_thumbnail("front-door", "snapshot-event", &jpeg)?;
    let path = events
        .thumbnail_path("front-door", "snapshot-event")?
        .expect("saved thumbnail resolves");
    assert!(path.starts_with(fs::canonicalize(root.join("named"))?));
    let decoded = image::open(&path)?;
    assert!(decoded.width() <= 384 && decoded.height() <= 216);
    assert_eq!(fs::read_dir(&config.event_thumbnail_path)?.count(), 0);
    drop(events);
    drop(config);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn thumbnail_placement_uses_the_camera_group_policy() -> anyhow::Result<()> {
    let (root, catalog, mut config) = fixture(1024 * 1024, true)?;
    let configuration = config.named_volumes.as_mut().unwrap();
    configuration.placement[0].source = None;
    configuration.placement[0].group = Some("entrances".into());
    config.volume_runtime = Some(Arc::new(Manager::new(
        configuration.clone(),
        catalog.handle(),
    )?));
    config
        .volume_groups
        .write()
        .unwrap()
        .insert("front-door".into(), vec!["entrances".into()]);
    let events = store(&catalog, &config)?;
    let jpeg = encode_jpeg(&DynamicImage::new_rgb8(24, 16))?;
    events.insert(image_event("group-event", &jpeg))?;
    events.save_thumbnail("front-door", "group-event", &jpeg)?;
    let path = events.thumbnail_path("front-door", "group-event")?.unwrap();
    assert!(path.starts_with(fs::canonicalize(root.join("named"))?));
    assert_eq!(fs::read_dir(&config.event_thumbnail_path)?.count(), 0);
    drop(events);
    drop(config);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn published_image_remains_on_its_volume_after_restart_and_default_change() -> anyhow::Result<()> {
    let (root, catalog, mut config) = fixture(1024 * 1024, true)?;
    let events = store(&catalog, &config)?;
    let jpeg = encode_jpeg(&DynamicImage::new_rgb8(24, 16))?;
    let event = image_event("published-event", &jpeg);
    assert_eq!(
        events.commit_published_image("publication-1", event.clone(), &jpeg)?,
        PublishedImageCommit::Stored
    );
    let original = events.thumbnail_path("front-door", &event.id)?.unwrap();
    assert!(original.starts_with(fs::canonicalize(root.join("named"))?));
    drop(events);
    config.volume_runtime = None;
    catalog.shutdown();
    let catalog = RecordingCatalog::open(&root.join("catalog.db"))?;
    config.event_thumbnail_path = root.join("changed-default");
    config.volume_runtime = Some(Arc::new(Manager::new(
        config.named_volumes.clone().unwrap(),
        catalog.handle(),
    )?));
    let events = store(&catalog, &config)?;
    let resolved = events.thumbnail_path("front-door", &event.id)?.unwrap();
    assert_eq!(resolved, original);
    assert_eq!(fs::read(resolved)?, jpeg);
    assert_eq!(
        events.commit_published_image("publication-1", event, &jpeg)?,
        PublishedImageCommit::Existing
    );
    assert_eq!(fs::read_dir(&config.event_thumbnail_path)?.count(), 0);
    drop(events);
    drop(config);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

fn rejected_destination_preserves_legacy(limit: u64, online: bool) -> anyhow::Result<()> {
    let (root, catalog, config) = fixture(limit, online)?;
    let events = store(&catalog, &config)?;
    let jpeg = encode_jpeg(&DynamicImage::new_rgb8(24, 16))?;
    events.insert(image_event("snapshot-event", &jpeg))?;
    assert!(
        events
            .save_thumbnail("front-door", "snapshot-event", &jpeg)
            .is_err()
    );
    assert!(
        events
            .thumbnail_path("front-door", "snapshot-event")?
            .is_none()
    );
    assert!(
        events
            .commit_published_image(
                "publication-1",
                image_event("published-event", &jpeg),
                &jpeg
            )
            .is_err()
    );
    assert!(events.event_by_id("published-event")?.is_none());
    assert_eq!(fs::read_dir(&config.event_thumbnail_path)?.count(), 0);
    if !online {
        assert!(!root.join("named").exists());
    }
    drop(events);
    drop(config);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn full_matched_volume_never_falls_back_to_the_legacy_directory() -> anyhow::Result<()> {
    rejected_destination_preserves_legacy(1, true)
}

#[test]
fn offline_matched_volume_never_falls_back_to_the_legacy_directory() -> anyhow::Result<()> {
    rejected_destination_preserves_legacy(1024 * 1024, false)
}

#[test]
fn failed_revision_keeps_the_current_published_image() -> anyhow::Result<()> {
    let jpeg = encode_jpeg(&DynamicImage::new_rgb8(24, 16))?;
    let (root, catalog, config) = fixture(u64::try_from(jpeg.len())?, true)?;
    let events = store(&catalog, &config)?;
    let event = image_event("published-event", &jpeg);
    events.commit_published_image("publication-1", event.clone(), &jpeg)?;
    let original = events.thumbnail_path("front-door", &event.id)?.unwrap();
    let replacement = encode_jpeg(&DynamicImage::new_rgb8(48, 32))?;
    let mut next = image_event(&event.id, &replacement);
    next.revision = 2;
    assert!(
        events
            .commit_published_image("publication-2", next, &replacement)
            .is_err()
    );
    assert_eq!(events.event_by_id(&event.id)?.unwrap().revision, 1);
    assert_eq!(
        events.thumbnail_path("front-door", &event.id)?.unwrap(),
        original
    );
    assert_eq!(fs::read(original)?, jpeg);
    assert_eq!(fs::read_dir(&config.event_thumbnail_path)?.count(), 0);
    drop(events);
    drop(config);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn legacy_revision_replaces_a_named_image_after_policy_stops_matching() -> anyhow::Result<()> {
    let (root, catalog, mut config) = fixture(1024 * 1024, true)?;
    let events = store(&catalog, &config)?;
    let first = encode_jpeg(&DynamicImage::new_rgb8(24, 16))?;
    events.commit_published_image("publication-1", image_event("event", &first), &first)?;
    let original = events.thumbnail_path("front-door", "event")?.unwrap();
    assert!(original.starts_with(fs::canonicalize(root.join("named"))?));
    let unrelated = config
        .event_thumbnail_path
        .join(original.file_name().unwrap());
    fs::write(&unrelated, &first)?;
    let configuration = config.named_volumes.as_mut().unwrap();
    configuration.placement.clear();
    config.volume_runtime = Some(Arc::new(Manager::new(
        configuration.clone(),
        catalog.handle(),
    )?));
    let events = events.with_volume_storage(&config);
    let second = encode_jpeg(&DynamicImage::new_rgb8(48, 32))?;
    let mut revision = image_event("event", &second);
    revision.revision = 2;
    events.commit_published_image("publication-2", revision.clone(), &second)?;
    let current = events.thumbnail_path("front-door", "event")?.unwrap();
    assert!(current.starts_with(fs::canonicalize(&config.event_thumbnail_path)?));
    assert_ne!(current, original);
    assert_eq!(fs::read(current)?, second);
    assert_eq!(fs::read(unrelated)?, first);
    assert_eq!(
        events.commit_published_image("publication-2", revision, &second)?,
        PublishedImageCommit::Existing
    );
    drop(events);
    drop(config);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn detached_named_thumbnail_is_no_longer_resolved() -> anyhow::Result<()> {
    let (root, catalog, config) = fixture(1024 * 1024, true)?;
    let events = store(&catalog, &config)?;
    let jpeg = encode_jpeg(&DynamicImage::new_rgb8(24, 16))?;
    events.commit_published_image("publication-1", image_event("event", &jpeg), &jpeg)?;
    assert!(events.thumbnail_path("front-door", "event")?.is_some());
    catalog.handle().detach_event_thumbnail("event")?;
    assert!(
        events
            .event_by_id("event")?
            .unwrap()
            .thumbnail_filename
            .is_none()
    );
    assert!(events.thumbnail_path("front-door", "event")?.is_none());
    assert!(
        events
            .attachment_path("front-door", "event", "snapshot")?
            .is_none()
    );
    drop(events);
    drop(config);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn native_legacy_replacement_preserves_unrelated_old_attachment_filename() -> anyhow::Result<()> {
    let (root, catalog, mut config) = fixture(1024 * 1024, true)?;
    let events = store(&catalog, &config)?;
    let jpeg = encode_jpeg(&DynamicImage::new_rgb8(24, 16))?;
    let mut event = image_event("native-event", &jpeg);
    event.source = crate::storage::metadata::EventSource::Camera;
    event.attachments[0].id = "isapi-old".into();
    event.canonical_attachment_id = Some("isapi-old".into());
    event.bbox_attachment_id = Some("isapi-old".into());
    events.commit_native_images(
        event.clone(),
        &[("isapi-old".into(), Arc::from(jpeg.clone()))],
    )?;
    let unrelated = config
        .event_thumbnail_path
        .join("native-event--isapi-old.jpg");
    fs::write(&unrelated, &jpeg)?;
    let configuration = config.named_volumes.as_mut().unwrap();
    configuration.placement.clear();
    config.volume_runtime = Some(Arc::new(Manager::new(
        configuration.clone(),
        catalog.handle(),
    )?));
    let events = events.with_volume_storage(&config);
    event.attachments[0].id = "isapi-new".into();
    event.canonical_attachment_id = Some("isapi-new".into());
    event.bbox_attachment_id = Some("isapi-new".into());
    events.commit_native_images(event, &[("isapi-new".into(), Arc::from(jpeg.clone()))])?;
    assert_eq!(fs::read(unrelated)?, jpeg);
    drop(events);
    drop(config);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn native_canonical_and_secondary_images_use_the_named_volume() -> anyhow::Result<()> {
    let (root, catalog, config) = fixture(1024 * 1024, true)?;
    let events = store(&catalog, &config)?;
    let first = encode_jpeg(&DynamicImage::new_rgb8(24, 16))?;
    let second = encode_jpeg(&DynamicImage::new_rgb8(48, 32))?;
    let mut event = image_event("native-event", &first);
    event.source = crate::storage::metadata::EventSource::Camera;
    event.attachments[0].id = "isapi-scene".into();
    event.canonical_attachment_id = Some("isapi-scene".into());
    event.bbox_attachment_id = Some("isapi-scene".into());
    let mut secondary = event.attachments[0].clone();
    secondary.id = "isapi-face".into();
    secondary.ordinal = 1;
    secondary.byte_len = Some(u64::try_from(second.len())?);
    event.attachments.push(secondary);
    let stored = events.commit_native_images(
        event,
        &[
            ("isapi-scene".into(), Arc::from(first.clone())),
            ("isapi-face".into(), Arc::from(second.clone())),
        ],
    )?;
    assert_eq!(stored.attachments.len(), 2);
    let canonical = events.thumbnail_path("front-door", &stored.id)?.unwrap();
    let canonical_attachment = events
        .attachment_path("front-door", &stored.id, "isapi-scene")?
        .unwrap();
    let secondary = events
        .attachment_path("front-door", &stored.id, "isapi-face")?
        .unwrap();
    let named = fs::canonicalize(root.join("named"))?;
    assert_eq!(canonical, canonical_attachment);
    assert!(canonical.starts_with(&named) && secondary.starts_with(&named));
    assert_ne!(canonical, secondary);
    assert_eq!(fs::read(canonical)?, first);
    assert_eq!(fs::read(secondary)?, second);
    assert_eq!(fs::read_dir(&config.event_thumbnail_path)?.count(), 0);
    assert!(
        events
            .attachment_path("another-camera", &stored.id, "isapi-face")?
            .is_none()
    );
    drop(events);
    drop(config);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn thumbnail_move_preserves_the_source_until_its_reader_releases() -> anyhow::Result<()> {
    use crate::storage::{
        catalog::locations::{Reply, Request},
        volumes::PlacementRequest,
    };

    let (root, catalog, mut config) = fixture(1024 * 1024, true)?;
    let events = store(&catalog, &config)?;
    let jpeg = encode_jpeg(&DynamicImage::new_rgb8(24, 16))?;
    events.commit_published_image("publication-1", image_event("event", &jpeg), &jpeg)?;
    let event = events.event_by_id("event")?.unwrap();
    let (source_path, lease) = events.leased_attachment_path(&event, "snapshot")?.unwrap();
    assert!(lease.is_some(), "named images must acquire a reader lease");
    let Reply::Location(Some(source)) = catalog.handle().volume_location(Request::Image {
        event: event.id.clone(),
        attachment: "snapshot".into(),
    })?
    else {
        anyhow::bail!("published image location is missing");
    };
    let secondary = root.join("secondary");
    protected_directory(&secondary)?;
    let configuration = config.named_volumes.as_mut().unwrap();
    let mut destination = configuration.volumes[0].clone();
    destination.id = VolumeId::parse("secondary")?;
    destination.root = secondary.clone();
    configuration.placement[0].source = None;
    configuration.placement[0].candidates = vec![destination.id.clone()];
    configuration.volumes.push(destination);
    let manager = Arc::new(Manager::new(configuration.clone(), catalog.handle())?);
    config.volume_runtime = Some(Arc::clone(&manager));
    let events = events.with_volume_storage(&config);
    let job_id = uuid::Uuid::new_v4().to_string();
    assert!(manager.move_object(
        &job_id,
        source.object,
        &PlacementRequest {
            role: VolumeRole::Thumbnail,
            source: "front-door",
            group: "",
            required_bytes: source.bytes,
        },
        &[],
        || false,
    )?);
    let destination_path = events.thumbnail_path("front-door", &event.id)?.unwrap();
    assert!(destination_path.starts_with(fs::canonicalize(secondary)?));
    assert!(!manager.retire_move(&job_id)?);
    assert_eq!(fs::read(&source_path)?, jpeg);
    assert_eq!(fs::read(&destination_path)?, jpeg);
    drop(lease);
    assert!(manager.retire_move(&job_id)?);
    assert!(!source_path.exists());
    let current = events.thumbnail_path("front-door", &event.id)?.unwrap();
    assert_eq!(current, destination_path);
    assert_eq!(fs::read(current)?, jpeg);
    assert_moved_image_retirement(&catalog, &manager, &events, &event, &destination_path)?;
    drop(events);
    drop(manager);
    drop(config);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

fn assert_moved_image_retirement(
    catalog: &RecordingCatalog,
    manager: &Manager,
    events: &EventStore,
    event: &crate::storage::metadata::TimelineEvent,
    path: &Path,
) -> anyhow::Result<()> {
    use crate::storage::catalog::locations::{Reply, Request};
    assert_eq!(
        catalog
            .handle()
            .volume_location(Request::ImagePressure("secondary".into()))?,
        Reply::ImagePressure(true)
    );
    let Reply::Location(Some(location)) = catalog.handle().volume_location(Request::Image {
        event: event.id.clone(),
        attachment: "snapshot".into(),
    })?
    else {
        anyhow::bail!("owned image location missing")
    };
    let Reply::ImageRetirement(Some(job)) = catalog
        .handle()
        .volume_location(Request::ImageRetirement(location.object.id))?
    else {
        anyhow::bail!("image retirement missing")
    };
    manager.retire_unused_image(&job.operation)?;
    assert!(!path.exists());
    assert!(events.thumbnail_path("front-door", &event.id).is_err());
    assert_eq!(image_allocated_bytes(catalog)?, 0);
    Ok(())
}

fn image_allocated_bytes(catalog: &RecordingCatalog) -> anyhow::Result<u64> {
    use crate::storage::catalog::locations::{Reply, Request};
    let Reply::Usage(usage) = catalog.handle().volume_location(Request::Usage)? else {
        anyhow::bail!("volume usage reply is missing");
    };
    Ok(usage.iter().map(|volume| volume.allocated_bytes).sum())
}

#[test]
fn rejected_named_image_commit_releases_its_reservation() -> anyhow::Result<()> {
    let (root, catalog, config) = fixture(1024 * 1024, true)?;
    let events = store(&catalog, &config)?;
    let jpeg = encode_jpeg(&DynamicImage::new_rgb8(24, 16))?;
    events.commit_published_image("publication-1", image_event("event", &jpeg), &jpeg)?;
    let current = events.event_by_id("event")?.unwrap();
    let path = events.thumbnail_path("front-door", "event")?.unwrap();
    assert!(
        events
            .commit_named_images(current, None, &[("snapshot".into(), &jpeg)])
            .is_err()
    );
    assert_eq!(image_allocated_bytes(&catalog)?, jpeg.len() as u64);
    assert_eq!(fs::read(path)?, jpeg);
    drop(events);
    drop(config);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn abandoned_image_receipt_is_immutable_and_fences_publication() -> anyhow::Result<()> {
    use crate::storage::catalog::locations::{Kind, Object, Reply, Request};
    use std::io::Write;
    let (root, catalog, config) = fixture(1024 * 1024, true)?;
    let manager = config.volume_runtime.as_ref().unwrap();
    let jpeg = encode_jpeg(&DynamicImage::new_rgb8(24, 16))?;
    let mut writer = manager
        .reserve(
            VolumeRole::Thumbnail,
            "front-door",
            &[],
            Object {
                kind: Kind::Thumbnail,
                id: uuid::Uuid::new_v4().to_string(),
            },
            jpeg.len() as u64,
        )?
        .unwrap()
        .open()?;
    writer.write_all(&jpeg)?;
    let evidence = writer.seal_image()?;
    let handle = catalog.handle();
    assert_eq!(
        handle.volume_location(Request::ImageAbandoned(evidence.clone()))?,
        Reply::Bound
    );
    assert!(writer.publish(evidence.clone()).is_err());
    drop(writer);
    assert!(manager.retire_unused_image(&evidence.operation)?);
    assert_eq!(image_allocated_bytes(&catalog)?, 0);
    assert_eq!(
        handle.volume_location(Request::ImageAbandoned(evidence.clone()))?,
        Reply::Bound
    );
    let mut changed = evidence;
    changed.digest[0] ^= 1;
    assert!(
        handle
            .volume_location(Request::ImageAbandoned(changed))
            .is_err()
    );
    drop(config);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn replacement_cleanup_waits_for_readers_and_releases_only_owned_bytes() -> anyhow::Result<()> {
    use crate::storage::volumes::runtime::worker::Worker;
    use std::time::{Duration, Instant};

    let (root, catalog, mut config) = fixture(1024 * 1024, true)?;
    let worker = Worker::start(config.volume_runtime.as_ref().unwrap().as_ref().clone())?;
    let handle = worker.handle();
    config.volume_mover = Some(handle.clone());
    let events = store(&catalog, &config)?;
    let first = encode_jpeg(&DynamicImage::new_rgb8(24, 16))?;
    events.commit_published_image("publication-1", image_event("event", &first), &first)?;
    let event = events.event_by_id("event")?.unwrap();
    let (old_path, lease) = events.leased_attachment_path(&event, "snapshot")?.unwrap();
    assert!(lease.is_some());
    let unrelated = root
        .join("named")
        .join(format!("{}.jpg", uuid::Uuid::new_v4()));
    fs::write(&unrelated, &first)?;
    let second = encode_jpeg(&DynamicImage::new_rgb8(48, 32))?;
    let mut next = image_event("event", &second);
    next.revision = 2;
    events.commit_published_image("publication-2", next, &second)?;
    let current = events.thumbnail_path("front-door", "event")?.unwrap();
    assert_ne!(old_path, current);
    handle.scan()?;
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(fs::read(&old_path)?, first);
    assert_eq!(
        image_allocated_bytes(&catalog)?,
        u64::try_from(first.len() + second.len())?
    );
    drop(lease);
    handle.scan()?;
    let expected = u64::try_from(second.len())?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while old_path.exists() || image_allocated_bytes(&catalog)? != expected {
        anyhow::ensure!(
            Instant::now() < deadline,
            "old image cleanup did not complete"
        );
        handle.scan()?;
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(fs::read(&current)?, second);
    assert_eq!(fs::read(unrelated)?, first);
    assert_eq!(
        events.thumbnail_path("front-door", "event")?.unwrap(),
        current
    );
    assert_eq!(image_allocated_bytes(&catalog)?, expected);
    worker.shutdown()?;
    drop(events);
    drop(config);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}
