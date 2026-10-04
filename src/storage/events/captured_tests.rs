use super::{EventStore, PublishedImageCommit, encode_jpeg, tests::image_event};
use crate::storage::catalog::{
    RecordingCatalog,
    locations::{Reply, Request, legacy::LegacyPaths},
};
use image::DynamicImage;
use std::{fs, path::Path};

fn capture_thumbnail_root(catalog: &RecordingCatalog, root: &Path) -> anyhow::Result<()> {
    let paths = LegacyPaths {
        active_root: root.join("active"),
        archive_root: root.join("archive"),
        export_root: root.join("archive/.exports"),
        thumbnail_root: root.join("thumbnails"),
        catalog_path: root.join("catalog.db"),
        export_history_path: root.join("archive/.exports/history.json"),
    };
    assert_eq!(
        catalog
            .handle()
            .volume_location(Request::RegisterLegacyPaths(Box::new(paths.clone())))?,
        Reply::LegacyPaths(Some(Box::new(paths)))
    );
    Ok(())
}

fn native_attachment(store: &EventStore, root: &Path, jpeg: &[u8]) -> anyhow::Result<()> {
    let mut event = image_event("native", jpeg);
    event.source = crate::storage::metadata::EventSource::Camera;
    let mut context = event.attachments[0].clone();
    context.id = "isapi-context".into();
    context.ordinal = 1;
    event.attachments.push(context);
    store.insert(event)?;
    fs::write(root.join("native--isapi-context.jpg"), jpeg)?;
    assert!(
        store
            .attachment_path("front-door", "native", "isapi-context")?
            .is_some()
    );
    Ok(())
}

#[test]
fn captured_offline_thumbnail_root_is_not_recreated_or_pruned_at_startup() -> anyhow::Result<()> {
    let root =
        std::env::temp_dir().join(format!("keeppeek-captured-images-{}", uuid::Uuid::new_v4()));
    let catalog_path = root.join("catalog.db");
    let thumbnail_root = root.join("thumbnails");
    let offline_root = root.join("offline-thumbnails");
    let catalog = RecordingCatalog::open(&catalog_path)?;
    let store = EventStore::new(catalog.handle(), &thumbnail_root, 0)?;
    let jpeg = encode_jpeg(&DynamicImage::new_rgb8(16, 16))?;
    assert_eq!(
        store.commit_published_image(
            "publication-retained",
            image_event("retained", &jpeg),
            &jpeg,
        )?,
        PublishedImageCommit::Stored
    );
    let before = store.event_by_id("retained")?.unwrap();
    native_attachment(&store, &thumbnail_root, &jpeg)?;
    let filename = before.thumbnail_filename.as_ref().unwrap();
    assert_eq!(fs::read(thumbnail_root.join(filename))?, jpeg);
    assert_eq!(
        before.attachments[0].byte_len,
        Some(u64::try_from(jpeg.len())?)
    );
    capture_thumbnail_root(&catalog, &root)?;
    drop(store);
    catalog.shutdown();
    fs::rename(&thumbnail_root, &offline_root)?;
    let catalog = RecordingCatalog::open(&catalog_path)?;
    #[cfg(windows)]
    let requested_root =
        std::path::PathBuf::from(thumbnail_root.to_string_lossy().to_ascii_uppercase());
    #[cfg(not(windows))]
    let requested_root = thumbnail_root.clone();
    // ponytail: a one-byte quota exercises startup pruning without a second retention fixture.
    let reopened = EventStore::new(catalog.handle(), &requested_root, 1)?;
    assert!(
        !thumbnail_root.exists(),
        "startup recreated a captured offline root"
    );
    assert_eq!(reopened.event_by_id("retained")?, Some(before.clone()));
    assert_eq!(reopened.thumbnail_path("front-door", "retained")?, None);
    assert_eq!(fs::read(offline_root.join(filename))?, jpeg);
    assert_eq!(reopened.events_in_range("front-door", 0, 2000)?.len(), 2);
    fs::rename(&offline_root, &thumbnail_root)?;
    let restored = reopened.thumbnail_path("front-door", "retained")?.unwrap();
    assert_eq!(fs::read(&restored)?, jpeg);
    assert_eq!(
        reopened.attachment_path("front-door", "retained", &before.attachments[0].id)?,
        Some(restored)
    );
    let native = reopened
        .attachment_path("front-door", "native", "isapi-context")?
        .unwrap();
    assert_eq!(fs::read(native)?, jpeg);
    drop(reopened);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn uncaptured_thumbnail_root_is_created_and_accepts_images() -> anyhow::Result<()> {
    let root = std::env::temp_dir().join(format!(
        "keeppeek-uncaptured-images-{}",
        uuid::Uuid::new_v4()
    ));
    let thumbnail_root = root.join("new/thumbnails");
    let catalog = RecordingCatalog::open(&root.join("catalog.db"))?;
    assert!(!thumbnail_root.exists());
    let jpeg = encode_jpeg(&DynamicImage::new_rgb8(16, 16))?;
    let store = EventStore::new(
        catalog.handle(),
        &thumbnail_root,
        u64::try_from(jpeg.len())? + 1,
    )?;
    assert!(thumbnail_root.is_dir());
    assert_eq!(
        catalog.handle().volume_location(Request::LegacyPaths)?,
        Reply::LegacyPaths(None)
    );
    assert_eq!(
        store.commit_published_image(
            "publication-created",
            image_event("created", &jpeg),
            &jpeg,
        )?,
        PublishedImageCommit::Stored
    );
    let path = store.thumbnail_path("front-door", "created")?.unwrap();
    assert_eq!(fs::read(path)?, jpeg);
    assert!(
        store
            .event_by_id("created")?
            .unwrap()
            .thumbnail_filename
            .is_some()
    );
    drop(store);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn offline_alias_root_restores_reads_without_following_later_replacement() -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;
    let temporary =
        std::env::temp_dir().join(format!("keeppeek-image-alias-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(temporary.join("real"))?;
    let root = temporary.canonicalize()?;
    let real = root.join("real");
    let alias = root.join("alias");
    symlink(&real, &alias)?;
    let images = real.join("thumbnails");
    let offline = real.join("offline");
    let catalog = RecordingCatalog::open(&root.join("catalog.db"))?;
    let store = EventStore::new(catalog.handle(), &alias.join("thumbnails"), 0)?;
    let jpeg = encode_jpeg(&DynamicImage::new_rgb8(16, 16))?;
    store.commit_published_image("alias-image", image_event("retained", &jpeg), &jpeg)?;
    native_attachment(&store, &images, &jpeg)?;
    let event = store.event_by_id("retained")?.unwrap();
    let filename = event.thumbnail_filename.as_ref().unwrap();
    capture_thumbnail_root(&catalog, &alias)?;
    drop(store);
    fs::rename(&images, &offline)?;
    let reopened = EventStore::new(catalog.handle(), &alias.join("thumbnails"), 1)?;
    assert!(!images.exists());
    assert_eq!(reopened.thumbnail_path("front-door", "retained")?, None);
    fs::rename(&offline, &images)?;
    let restored = reopened
        .thumbnail_path("front-door", "retained")?
        .expect("restored alias-root image must resolve");
    assert_eq!(restored, images.join(filename).canonicalize()?);
    assert_eq!(fs::read(&restored)?, jpeg);
    let context = reopened
        .attachment_path("front-door", "native", "isapi-context")?
        .expect("restored noncanonical native image must resolve");
    assert_eq!(fs::read(context)?, jpeg);
    assert_eq!(reopened.event_by_id("retained")?, Some(event.clone()));
    let outside = root.join("outside");
    fs::create_dir(&outside)?;
    fs::write(outside.join(filename), &jpeg)?;
    fs::write(outside.join("native--isapi-context.jpg"), &jpeg)?;
    fs::rename(&images, &offline)?;
    symlink(&outside, &images)?;
    assert_eq!(reopened.thumbnail_path("front-door", "retained")?, None);
    assert_eq!(
        reopened.attachment_path("front-door", "native", "isapi-context")?,
        None
    );
    fs::remove_file(&images)?;
    fs::remove_file(&alias)?;
    drop(reopened);
    catalog.shutdown();
    fs::remove_dir_all(root)?;
    Ok(())
}
