use crate::{
    operational_events::OperationalEvent,
    storage::{RecordingCatalogHandle, catalog::EventPublicationIdentity, metadata::TimelineEvent},
};
use image::{DynamicImage, ImageFormat, codecs::jpeg::JpegEncoder};
use reo_proto::MAX_SNAPSHOT_BYTES;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    time::SystemTime,
};

const THUMBNAIL_WIDTH: u32 = 384;
const THUMBNAIL_HEIGHT: u32 = 216;
const JPEG_QUALITY: u8 = 82;
const PUBLISHED_IMAGE_DIMENSION_MAX: u32 = 8_192;
const PUBLISHED_IMAGE_ALLOCATION_MAX: u64 = 64 * 1024 * 1024;

#[derive(Clone)]
pub struct EventStore {
    catalog: RecordingCatalogHandle,
    thumbnail_root: PathBuf,
    max_thumbnail_bytes: u64,
}

#[derive(Debug)]
pub(crate) enum PublishedImageCommitError {
    Invalid(anyhow::Error),
    Conflict(Option<u64>),
    Storage(anyhow::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PublishedImageCommit {
    Stored,
    Existing,
}

impl std::fmt::Display for PublishedImageCommitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(error) => write!(formatter, "invalid published image: {error}"),
            Self::Conflict(Some(revision)) => {
                write!(
                    formatter,
                    "published image conflicts with revision {revision}"
                )
            }
            Self::Conflict(None) => formatter.write_str("published image revision conflicts"),
            Self::Storage(error) => write!(formatter, "published image storage failed: {error}"),
        }
    }
}

impl std::error::Error for PublishedImageCommitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Invalid(error) | Self::Storage(error) => Some(error.as_ref()),
            Self::Conflict(_) => None,
        }
    }
}

impl EventStore {
    pub fn new(
        catalog: RecordingCatalogHandle,
        thumbnail_root: &Path,
        max_thumbnail_bytes: u64,
    ) -> anyhow::Result<Self> {
        fs::create_dir_all(thumbnail_root)?;
        let store = Self {
            catalog,
            thumbnail_root: thumbnail_root.canonicalize()?,
            max_thumbnail_bytes,
        };
        store.reconcile_image_files()?;
        store.enforce_thumbnail_limit()?;
        Ok(store)
    }

    pub fn insert(&self, event: TimelineEvent) -> anyhow::Result<()> {
        self.catalog.insert_event(event)
    }

    pub fn close(&self, id: &str, end_time_ms: i64) -> anyhow::Result<()> {
        self.catalog.close_event(id, end_time_ms)
    }

    pub fn events_in_range(
        &self,
        camera_id: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> anyhow::Result<Vec<TimelineEvent>> {
        self.catalog.events_in_range(camera_id, start_ms, end_ms)
    }

    pub fn event_by_id(&self, id: &str) -> anyhow::Result<Option<TimelineEvent>> {
        self.catalog.event_by_id(id)
    }

    pub(crate) fn event_publication_identity(
        &self,
        id: &str,
    ) -> anyhow::Result<Option<EventPublicationIdentity>> {
        self.catalog.event_publication_identity(id)
    }

    pub(crate) fn upsert_operational_event(&self, event: OperationalEvent) -> anyhow::Result<()> {
        self.catalog.upsert_operational_event(event)
    }

    pub(crate) fn operational_events_in_range(
        &self,
        camera_id: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> anyhow::Result<Vec<OperationalEvent>> {
        self.catalog
            .operational_events_in_range(camera_id, start_ms, end_ms)
    }

    pub(crate) fn open_operational_events(&self) -> anyhow::Result<Vec<OperationalEvent>> {
        self.catalog.open_operational_events()
    }

    pub fn save_thumbnail(
        &self,
        camera_id: &str,
        event_id: &str,
        jpeg: &[u8],
    ) -> anyhow::Result<()> {
        if !safe_event_id(event_id) {
            anyhow::bail!("invalid event identifier");
        }
        if jpeg.len() > MAX_SNAPSHOT_BYTES {
            anyhow::bail!("event snapshot exceeds {MAX_SNAPSHOT_BYTES} bytes");
        }
        let event = self
            .catalog
            .event_by_id(event_id)?
            .ok_or_else(|| anyhow::anyhow!("event was not found"))?;
        if event.camera_id != camera_id {
            anyhow::bail!("event does not belong to camera");
        }

        let decoded = image::load_from_memory_with_format(jpeg, ImageFormat::Jpeg)?;
        let thumbnail = decoded.thumbnail(THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT);
        let encoded = encode_jpeg(&thumbnail)?;
        let byte_len = u64::try_from(encoded.len())?;
        let filename = format!("{event_id}.jpg");
        let destination = self.thumbnail_root.join(&filename);
        let temporary = self.thumbnail_root.join(format!(".{event_id}.tmp"));
        fs::write(&temporary, encoded)?;
        fs::rename(&temporary, &destination)?;
        if let Err(error) = self
            .catalog
            .attach_event_thumbnail(event_id, &filename, byte_len)
        {
            let _ = fs::remove_file(destination);
            return Err(error);
        }
        self.enforce_thumbnail_limit()?;
        Ok(())
    }

    pub(crate) fn commit_published_image(
        &self,
        publication_id: &str,
        mut event: TimelineEvent,
        jpeg: &[u8],
    ) -> Result<PublishedImageCommit, PublishedImageCommitError> {
        use PublishedImageCommitError::{Invalid, Storage};

        if !safe_event_id(&event.id) || !safe_event_id(publication_id) {
            return Err(Invalid(anyhow::anyhow!(
                "invalid event or publication identifier"
            )));
        }
        let descriptor = event.canonical_attachment().ok_or_else(|| {
            Invalid(anyhow::anyhow!(
                "published event has no canonical attachment"
            ))
        })?;
        let jpeg_len = u64::try_from(jpeg.len()).map_err(|error| Invalid(error.into()))?;
        if descriptor.content_type != "image/jpeg" || descriptor.byte_len != Some(jpeg_len) {
            return Err(Invalid(anyhow::anyhow!(
                "published event image does not match its descriptor"
            )));
        }
        if jpeg.len() > MAX_SNAPSHOT_BYTES
            || (self.max_thumbnail_bytes != 0 && jpeg_len > self.max_thumbnail_bytes)
        {
            return Err(Invalid(anyhow::anyhow!(
                "published event image exceeds the storage limit"
            )));
        }
        validate_published_jpeg(jpeg).map_err(Invalid)?;

        let publication = EventPublicationIdentity {
            publication_id: publication_id.to_owned(),
            fingerprint: published_image_fingerprint(publication_id, &event, jpeg)
                .map_err(Invalid)?,
        };
        let previous = self.catalog.event_by_id(&event.id).map_err(Storage)?;
        match previous.as_ref() {
            Some(stored) if event.revision == stored.revision => {
                let stored_publication = self
                    .catalog
                    .event_publication_identity(&event.id)
                    .map_err(Storage)?;
                if stored_publication.as_ref() == Some(&publication) {
                    let filename = stored.thumbnail_filename.as_deref().ok_or_else(|| {
                        Storage(anyhow::anyhow!("published event image is missing"))
                    })?;
                    let stored_jpeg = fs::read(self.thumbnail_root.join(filename))
                        .map_err(|error| Storage(error.into()))?;
                    if stored_jpeg == jpeg {
                        return Ok(PublishedImageCommit::Existing);
                    }
                }
                return Err(PublishedImageCommitError::Conflict(Some(stored.revision)));
            }
            Some(stored)
                if event.revision < stored.revision
                    || event.camera_id != stored.camera_id
                    || event.source != stored.source =>
            {
                return Err(PublishedImageCommitError::Conflict(Some(stored.revision)));
            }
            None if event.revision != 1 => {
                return Err(PublishedImageCommitError::Conflict(None));
            }
            Some(_) | None => {}
        }
        let filename = format!("{}--r{}.jpg", event.id, event.revision);
        let destination = self.thumbnail_root.join(&filename);
        let temporary = self
            .thumbnail_root
            .join(format!(".publication-{}.tmp", uuid::Uuid::new_v4()));
        let promote = (|| -> std::io::Result<()> {
            crate::config::write_private_file(&temporary, jpeg)?;
            fs::File::open(&temporary)?.sync_all()?;
            if destination.exists() {
                fs::remove_file(&destination)?;
            }
            fs::rename(&temporary, &destination)?;
            fs::File::open(&destination)?.sync_all()?;
            #[cfg(unix)]
            sync_directory(&self.thumbnail_root)?;
            Ok(())
        })();
        if let Err(error) = promote {
            let _ = fs::remove_file(&temporary);
            return Err(Storage(error.into()));
        }
        event.thumbnail_filename = Some(filename.clone());
        if let Err(error) = self.catalog.insert_published_event(event, publication) {
            let _ = fs::remove_file(destination);
            return Err(Storage(error));
        }
        if let Some(previous_filename) = previous.and_then(|event| event.thumbnail_filename)
            && previous_filename != filename
            && safe_image_filename(&previous_filename)
        {
            let _ = fs::remove_file(self.thumbnail_root.join(previous_filename));
        }
        if let Err(error) = self.enforce_thumbnail_limit() {
            tracing::warn!(%error, "unable to enforce event image retention after commit");
        }
        Ok(PublishedImageCommit::Stored)
    }

    pub fn thumbnail_path(
        &self,
        camera_id: &str,
        event_id: &str,
    ) -> anyhow::Result<Option<PathBuf>> {
        if !safe_event_id(event_id) {
            return Ok(None);
        }
        let Some(event) = self.catalog.event_by_id(event_id)? else {
            return Ok(None);
        };
        if event.camera_id != camera_id {
            return Ok(None);
        }
        let Some(filename) = event.thumbnail_filename else {
            return Ok(None);
        };
        if !safe_image_filename(&filename) {
            return Ok(None);
        }
        let candidate = self.thumbnail_root.join(filename);
        let Ok(candidate) = candidate.canonicalize() else {
            return Ok(None);
        };
        if !candidate.starts_with(&self.thumbnail_root) {
            return Ok(None);
        }
        Ok(Some(candidate))
    }

    fn reconcile_image_files(&self) -> anyhow::Result<()> {
        let mut referenced = self
            .catalog
            .event_thumbnail_filenames()?
            .into_iter()
            .collect::<HashSet<_>>();
        for entry in fs::read_dir(&self.thumbnail_root)? {
            let entry = entry?;
            let filename = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("event image filename is not valid UTF-8"))?;
            let metadata = entry.metadata()?;
            if !metadata.is_file() {
                continue;
            }
            let interrupted = filename.starts_with('.') && filename.ends_with(".tmp");
            let orphaned = safe_image_filename(&filename) && !referenced.remove(&filename);
            if interrupted || orphaned {
                fs::remove_file(entry.path())?;
            }
        }
        for filename in referenced {
            self.catalog.detach_event_thumbnail_file(&filename)?;
        }
        Ok(())
    }

    fn enforce_thumbnail_limit(&self) -> anyhow::Result<()> {
        if self.max_thumbnail_bytes == 0 {
            return Ok(());
        }
        let mut thumbnails = fs::read_dir(&self.thumbnail_root)?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                let filename = path.file_name()?.to_str()?.to_owned();
                let metadata = entry.metadata().ok()?;
                safe_image_filename(&filename).then_some((
                    metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    filename,
                    path,
                    metadata.len(),
                ))
            })
            .collect::<Vec<_>>();
        let mut total = thumbnails.iter().map(|(_, _, _, bytes)| bytes).sum::<u64>();
        thumbnails.sort_unstable_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
        for (_, filename, path, bytes) in thumbnails {
            if total <= self.max_thumbnail_bytes {
                break;
            }
            self.catalog.detach_event_thumbnail_file(&filename)?;
            fs::remove_file(path)?;
            total = total.saturating_sub(bytes);
        }
        Ok(())
    }
}

fn encode_jpeg(image: &DynamicImage) -> anyhow::Result<Vec<u8>> {
    let mut encoded = Vec::new();
    JpegEncoder::new_with_quality(&mut encoded, JPEG_QUALITY).encode_image(image)?;
    Ok(encoded)
}

fn validate_published_jpeg(jpeg: &[u8]) -> anyhow::Result<()> {
    let mut reader = image::ImageReader::with_format(Cursor::new(jpeg), ImageFormat::Jpeg);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(PUBLISHED_IMAGE_DIMENSION_MAX);
    limits.max_image_height = Some(PUBLISHED_IMAGE_DIMENSION_MAX);
    limits.max_alloc = Some(PUBLISHED_IMAGE_ALLOCATION_MAX);
    reader.limits(limits);
    reader.decode()?;
    Ok(())
}

fn published_image_fingerprint(
    publication_id: &str,
    event: &TimelineEvent,
    jpeg: &[u8],
) -> anyhow::Result<String> {
    let metadata = serde_json::to_vec(&(1_u8, publication_id, event))?;
    let mut hasher = Sha256::new();
    hasher.update(u64::try_from(metadata.len())?.to_be_bytes());
    hasher.update(metadata);
    hasher.update(u64::try_from(jpeg.len())?.to_be_bytes());
    hasher.update(jpeg);
    Ok(encode_lower_hex(hasher.finalize()))
}

fn encode_lower_hex(bytes: impl AsRef<[u8]>) -> String {
    use std::fmt::Write as _;

    let bytes = bytes.as_ref();
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        write!(encoded, "{byte:02x}").expect("writing to a string cannot fail");
    }
    encoded
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    fs::File::open(path)?.sync_all()
}

fn safe_image_filename(filename: &str) -> bool {
    filename.ends_with(".jpg")
        && !matches!(filename, "." | "..")
        && filename
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn safe_event_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{RecordingCatalog, metadata::EventSource};

    fn jpeg_with_declared_dimensions(width: u16, height: u16) -> Vec<u8> {
        let mut jpeg = encode_jpeg(&DynamicImage::new_rgb8(16, 16)).unwrap();
        let sof = jpeg
            .windows(2)
            .position(|marker| marker == [0xff, 0xc0])
            .expect("test JPEG must contain a baseline start-of-frame marker");
        jpeg[sof + 5..sof + 7].copy_from_slice(&height.to_be_bytes());
        jpeg[sof + 7..sof + 9].copy_from_slice(&width.to_be_bytes());
        jpeg
    }

    #[test]
    fn published_jpeg_validation_bounds_dimensions_and_decoded_allocation() {
        let valid = encode_jpeg(&DynamicImage::new_rgb8(16, 16)).unwrap();
        validate_published_jpeg(&valid).unwrap();

        for jpeg in [
            jpeg_with_declared_dimensions(8_193, 16),
            jpeg_with_declared_dimensions(8_192, 8_192),
        ] {
            let error = validate_published_jpeg(&jpeg).unwrap_err();
            assert!(matches!(
                error.downcast_ref::<image::ImageError>(),
                Some(image::ImageError::Limits(_))
            ));
        }
    }

    #[test]
    fn published_image_revision_replaces_the_catalog_and_previous_file() {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-published-image-revision-{}",
            rand::random::<u64>()
        ));
        let thumbnail_root = root.join("thumbnails");
        let catalog = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
        let events = catalog.handle();
        let store = EventStore::new(events.clone(), &thumbnail_root, 0).unwrap();
        let first = encode_jpeg(&DynamicImage::new_rgb8(16, 16)).unwrap();
        let second = encode_jpeg(&DynamicImage::new_rgb8(24, 16)).unwrap();
        for (revision, jpeg) in [(1, &first), (2, &second)] {
            store
                .commit_published_image(
                    &format!("publication-{revision}"),
                    TimelineEvent {
                        id: "published-event".to_owned(),
                        revision,
                        camera_id: "front-door".to_owned(),
                        stream: Some("sub".to_owned()),
                        source: EventSource::KeepPeek,
                        kind: "person".to_owned(),
                        start_time_ms: 1_000,
                        end_time_ms: None,
                        confidence: Some(0.9),
                        bbox: None,
                        bbox_attachment_id: Some("snapshot".to_owned()),
                        zone: None,
                        text: None,
                        payload: None,
                        attachments: vec![crate::storage::metadata::EventAttachment {
                            id: "snapshot".to_owned(),
                            attachment_type: "snapshot".to_owned(),
                            content_type: "image/jpeg".to_owned(),
                            byte_len: Some(jpeg.len() as u64),
                            ordinal: 0,
                            timestamp_ms: Some(1_000),
                            text: None,
                        }],
                        canonical_attachment_id: Some("snapshot".to_owned()),
                        icon_key: "person".to_owned(),
                        rejected_icon_key: None,
                        thumbnail_filename: None,
                    },
                    jpeg,
                )
                .unwrap();
        }

        let stored = events.event_by_id("published-event").unwrap().unwrap();
        assert_eq!(stored.revision, 2);
        assert_eq!(stored.attachments[0].byte_len, Some(second.len() as u64));
        let mut stale = stored;
        stale.thumbnail_filename = None;
        stale.attachments[0].byte_len = Some(first.len() as u64);
        store
            .commit_published_image("publication-2", stale, &first)
            .unwrap_err();
        assert!(!thumbnail_root.join("published-event--r1.jpg").exists());
        assert_eq!(
            fs::read(thumbnail_root.join("published-event--r2.jpg")).unwrap(),
            second
        );

        drop(store);
        drop(events);
        catalog.shutdown();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn thumbnail_is_resized_and_requires_camera_ownership() {
        let root = std::env::temp_dir().join(format!("keeppeek-events-{}", rand::random::<u64>()));
        fs::create_dir_all(&root).unwrap();
        let catalog = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
        let store = EventStore::new(catalog.handle(), &root.join("thumbnails"), 0).unwrap();
        store
            .insert(TimelineEvent {
                id: "event-1".to_owned(),
                revision: 1,
                camera_id: "front-door".to_owned(),
                stream: Some("sub".to_owned()),
                source: EventSource::Camera,
                kind: "motion".to_owned(),
                start_time_ms: 1_000,
                end_time_ms: None,
                confidence: None,
                bbox: None,
                bbox_attachment_id: None,
                zone: None,
                text: None,
                payload: None,
                attachments: Vec::new(),
                canonical_attachment_id: None,
                icon_key: "motion".to_owned(),
                rejected_icon_key: None,
                thumbnail_filename: None,
            })
            .unwrap();

        let source = DynamicImage::new_rgb8(1_280, 720);
        let jpeg = encode_jpeg(&source).unwrap();
        store
            .save_thumbnail("front-door", "event-1", &jpeg)
            .unwrap();

        let path = store
            .thumbnail_path("front-door", "event-1")
            .unwrap()
            .unwrap();
        let thumbnail = image::open(path).unwrap();
        assert_eq!((thumbnail.width(), thumbnail.height()), (384, 216));
        assert!(
            store
                .thumbnail_path("back-yard", "event-1")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .thumbnail_path("front-door", "../event-1")
                .unwrap()
                .is_none()
        );

        drop(store);
        catalog.shutdown();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn thumbnail_accepts_snapshots_below_the_protocol_limit() {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-event-snapshot-limit-{}",
            rand::random::<u64>()
        ));
        fs::create_dir_all(&root).unwrap();
        let catalog = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
        let store = EventStore::new(catalog.handle(), &root.join("thumbnails"), 0).unwrap();

        let jpeg = vec![0; MAX_SNAPSHOT_BYTES - 1];
        let error = store
            .save_thumbnail("front-door", "event-1", &jpeg)
            .unwrap_err();
        assert_eq!(error.to_string(), "event was not found");

        drop(store);
        catalog.shutdown();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn thumbnail_limit_prunes_old_images_and_catalog_references() {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-thumbnail-limit-{}",
            rand::random::<u64>()
        ));
        let thumbnail_root = root.join("thumbnails");
        fs::create_dir_all(&root).unwrap();
        let catalog = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
        let events = catalog.handle();
        for (id, started_at_ms) in [("event-1", 1_000), ("event-2", 2_000)] {
            events
                .insert_event(TimelineEvent {
                    id: id.to_owned(),
                    revision: 1,
                    camera_id: "front-door".to_owned(),
                    stream: None,
                    source: EventSource::Camera,
                    kind: "motion".to_owned(),
                    start_time_ms: started_at_ms,
                    end_time_ms: Some(started_at_ms + 1),
                    confidence: None,
                    bbox: None,
                    bbox_attachment_id: None,
                    zone: None,
                    text: None,
                    payload: None,
                    attachments: Vec::new(),
                    canonical_attachment_id: None,
                    icon_key: "motion".to_owned(),
                    rejected_icon_key: None,
                    thumbnail_filename: None,
                })
                .unwrap();
        }
        let store = EventStore::new(events.clone(), &thumbnail_root, 0).unwrap();
        let jpeg = encode_jpeg(&DynamicImage::new_rgb8(1_280, 720)).unwrap();
        store
            .save_thumbnail("front-door", "event-1", &jpeg)
            .unwrap();
        store
            .save_thumbnail("front-door", "event-2", &jpeg)
            .unwrap();
        let retained_size = fs::metadata(thumbnail_root.join("event-2.jpg"))
            .unwrap()
            .len();

        let limited = EventStore::new(events, &thumbnail_root, retained_size).unwrap();

        assert!(
            limited
                .thumbnail_path("front-door", "event-1")
                .unwrap()
                .is_none()
        );
        assert!(
            limited
                .thumbnail_path("front-door", "event-2")
                .unwrap()
                .is_some()
        );
        let pruned = limited.event_by_id("event-1").unwrap().unwrap();
        assert_eq!(pruned.revision, 2);
        assert_eq!(pruned.canonical_attachment_id.as_deref(), Some("thumbnail"));
        assert_eq!(pruned.attachments[0].id, "thumbnail");
        assert!(pruned.thumbnail_filename.is_none());

        drop(limited);
        drop(store);
        catalog.shutdown();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn thumbnail_limit_prunes_published_revision_files_by_catalog_filename() {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-published-image-limit-{}",
            rand::random::<u64>()
        ));
        let thumbnail_root = root.join("thumbnails");
        let catalog = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
        let events = catalog.handle();
        let store = EventStore::new(events.clone(), &thumbnail_root, 0).unwrap();
        let jpeg = encode_jpeg(&DynamicImage::new_rgb8(16, 16)).unwrap();
        for (id, start_time_ms) in [("published-1", 1_000), ("published-2", 2_000)] {
            store
                .commit_published_image(
                    &format!("publication-{id}"),
                    TimelineEvent {
                        id: id.to_owned(),
                        revision: 1,
                        camera_id: "front-door".to_owned(),
                        stream: Some("sub".to_owned()),
                        source: EventSource::KeepPeek,
                        kind: "person".to_owned(),
                        start_time_ms,
                        end_time_ms: None,
                        confidence: None,
                        bbox: None,
                        bbox_attachment_id: Some("snapshot".to_owned()),
                        zone: None,
                        text: None,
                        payload: None,
                        attachments: vec![crate::storage::metadata::EventAttachment {
                            id: "snapshot".to_owned(),
                            attachment_type: "snapshot".to_owned(),
                            content_type: "image/jpeg".to_owned(),
                            byte_len: Some(jpeg.len() as u64),
                            ordinal: 0,
                            timestamp_ms: Some(start_time_ms),
                            text: None,
                        }],
                        canonical_attachment_id: Some("snapshot".to_owned()),
                        icon_key: "person".to_owned(),
                        rejected_icon_key: None,
                        thumbnail_filename: None,
                    },
                    &jpeg,
                )
                .unwrap();
        }
        let retained_size = fs::metadata(thumbnail_root.join("published-2--r1.jpg"))
            .unwrap()
            .len();
        drop(store);

        let limited = EventStore::new(events, &thumbnail_root, retained_size).unwrap();

        assert!(
            limited
                .thumbnail_path("front-door", "published-1")
                .unwrap()
                .is_none()
        );
        assert!(
            limited
                .thumbnail_path("front-door", "published-2")
                .unwrap()
                .is_some()
        );

        drop(limited);
        catalog.shutdown();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn startup_reconciles_orphaned_and_missing_event_images() {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-event-image-reconcile-{}",
            rand::random::<u64>()
        ));
        let thumbnail_root = root.join("thumbnails");
        let catalog = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
        let events = catalog.handle();
        let store = EventStore::new(events.clone(), &thumbnail_root, 0).unwrap();
        let jpeg = encode_jpeg(&DynamicImage::new_rgb8(16, 16)).unwrap();
        let attachment = crate::storage::metadata::EventAttachment {
            id: "snapshot".to_owned(),
            attachment_type: "snapshot".to_owned(),
            content_type: "image/jpeg".to_owned(),
            byte_len: Some(jpeg.len() as u64),
            ordinal: 0,
            timestamp_ms: Some(1_000),
            text: None,
        };
        store
            .commit_published_image(
                "publication-retained",
                TimelineEvent {
                    id: "retained".to_owned(),
                    revision: 1,
                    camera_id: "front-door".to_owned(),
                    stream: Some("sub".to_owned()),
                    source: EventSource::KeepPeek,
                    kind: "person".to_owned(),
                    start_time_ms: 1_000,
                    end_time_ms: None,
                    confidence: None,
                    bbox: None,
                    bbox_attachment_id: Some("snapshot".to_owned()),
                    zone: None,
                    text: None,
                    payload: None,
                    attachments: vec![attachment.clone()],
                    canonical_attachment_id: Some("snapshot".to_owned()),
                    icon_key: "person".to_owned(),
                    rejected_icon_key: None,
                    thumbnail_filename: None,
                },
                &jpeg,
            )
            .unwrap();
        events
            .insert_event(TimelineEvent {
                id: "missing".to_owned(),
                revision: 1,
                camera_id: "front-door".to_owned(),
                stream: Some("sub".to_owned()),
                source: EventSource::KeepPeek,
                kind: "person".to_owned(),
                start_time_ms: 2_000,
                end_time_ms: None,
                confidence: None,
                bbox: None,
                bbox_attachment_id: Some("snapshot".to_owned()),
                zone: None,
                text: None,
                payload: None,
                attachments: vec![attachment],
                canonical_attachment_id: Some("snapshot".to_owned()),
                icon_key: "person".to_owned(),
                rejected_icon_key: None,
                thumbnail_filename: Some("missing--r1.jpg".to_owned()),
            })
            .unwrap();
        fs::write(thumbnail_root.join("orphan--r1.jpg"), &jpeg).unwrap();
        fs::write(thumbnail_root.join(".publication-interrupted.tmp"), &jpeg).unwrap();
        drop(store);

        let reconciled = EventStore::new(events, &thumbnail_root, 0).unwrap();

        assert!(thumbnail_root.join("retained--r1.jpg").exists());
        assert!(!thumbnail_root.join("orphan--r1.jpg").exists());
        assert!(!thumbnail_root.join(".publication-interrupted.tmp").exists());
        assert!(
            reconciled
                .event_by_id("missing")
                .unwrap()
                .unwrap()
                .thumbnail_filename
                .is_none()
        );

        drop(reconciled);
        catalog.shutdown();
        fs::remove_dir_all(root).unwrap();
    }
}
