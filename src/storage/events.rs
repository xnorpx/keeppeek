use crate::storage::{RecordingCatalogHandle, metadata::TimelineEvent};
use image::{DynamicImage, ImageFormat, codecs::jpeg::JpegEncoder};
use reo_proto::MAX_SNAPSHOT_BYTES;
use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

const THUMBNAIL_WIDTH: u32 = 384;
const THUMBNAIL_HEIGHT: u32 = 216;
const JPEG_QUALITY: u8 = 82;

#[derive(Clone)]
pub struct EventStore {
    catalog: RecordingCatalogHandle,
    thumbnail_root: PathBuf,
    max_thumbnail_bytes: u64,
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
        if filename != format!("{event_id}.jpg") {
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

    fn enforce_thumbnail_limit(&self) -> anyhow::Result<()> {
        if self.max_thumbnail_bytes == 0 {
            return Ok(());
        }
        let mut thumbnails = fs::read_dir(&self.thumbnail_root)?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                let event_id = path.file_stem()?.to_str()?.to_owned();
                let metadata = entry.metadata().ok()?;
                (path.extension().and_then(|extension| extension.to_str()) == Some("jpg")
                    && safe_event_id(&event_id))
                .then_some((
                    metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    event_id,
                    path,
                    metadata.len(),
                ))
            })
            .collect::<Vec<_>>();
        let mut total = thumbnails.iter().map(|(_, _, _, bytes)| bytes).sum::<u64>();
        thumbnails.sort_unstable_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
        for (_, event_id, path, bytes) in thumbnails {
            if total <= self.max_thumbnail_bytes {
                break;
            }
            self.catalog.detach_event_thumbnail(&event_id)?;
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
}
