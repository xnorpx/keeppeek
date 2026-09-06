use std::collections::HashSet;
use std::sync::Arc;

use super::{
    EventStore, TimelineEvent, fs, safe_event_id, safe_image_filename, validate_published_jpeg,
};
use crate::storage::metadata::EventSource;

impl EventStore {
    pub(crate) fn commit_native_images(
        &self,
        mut event: TimelineEvent,
        images: &[(String, Arc<[u8]>)],
    ) -> anyhow::Result<TimelineEvent> {
        anyhow::ensure!(
            event.source == EventSource::Camera && safe_event_id(&event.id),
            "invalid native event identity"
        );
        validate_images(&event, images, self.max_thumbnail_bytes)?;
        let previous = self.catalog.event_by_id(&event.id)?;
        if let Some(previous) = &previous {
            anyhow::ensure!(
                previous.source == event.source && previous.camera_id == event.camera_id,
                "native event source changed"
            );
            event.revision = previous
                .revision
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("native event revision exhausted"))?;
            event.start_time_ms = previous.start_time_ms;
            event.end_time_ms = previous.end_time_ms;
            if images.is_empty() {
                event.attachments.clone_from(&previous.attachments);
                event
                    .canonical_attachment_id
                    .clone_from(&previous.canonical_attachment_id);
                event
                    .thumbnail_filename
                    .clone_from(&previous.thumbnail_filename);
                event.bbox = previous.bbox;
                event
                    .bbox_attachment_id
                    .clone_from(&previous.bbox_attachment_id);
            }
        }
        let mut created = Vec::new();
        let result = (|| {
            for (id, bytes) in images {
                let filename = native_filename(&event.id, id)?;
                if self.stage_native_image(&filename, bytes)? {
                    created.push(filename);
                }
            }
            if !images.is_empty() {
                event.thumbnail_filename = event
                    .canonical_attachment_id
                    .as_deref()
                    .map(|id| native_filename(&event.id, id))
                    .transpose()?;
            }
            self.catalog.insert_event(event.clone())
        })();
        if let Err(error) = result {
            for filename in created {
                let _ = fs::remove_file(self.thumbnail_root.join(filename));
            }
            return Err(error);
        }
        if let Some(previous) = previous {
            for descriptor in previous.attachments {
                if descriptor.id.starts_with("isapi-")
                    && !event
                        .attachments
                        .iter()
                        .any(|current| current.id == descriptor.id)
                {
                    let filename = native_filename(&event.id, &descriptor.id)?;
                    let _ = fs::remove_file(self.thumbnail_root.join(filename));
                }
            }
        }
        if let Err(error) = self.enforce_thumbnail_limit() {
            tracing::warn!(%error, "unable to enforce native event image retention after commit");
        }
        Ok(event)
    }

    pub fn attachment_path(
        &self,
        camera_id: &str,
        event_id: &str,
        attachment_id: &str,
    ) -> anyhow::Result<Option<std::path::PathBuf>> {
        let Some(event) = self.catalog.event_by_id(event_id)? else {
            return Ok(None);
        };
        if event.camera_id != camera_id
            || !event
                .attachments
                .iter()
                .any(|descriptor| descriptor.id == attachment_id)
        {
            return Ok(None);
        }
        if event.canonical_attachment_id.as_deref() == Some(attachment_id) {
            return self.thumbnail_path(camera_id, event_id);
        }
        if event.source != EventSource::Camera || !attachment_id.starts_with("isapi-") {
            return Ok(None);
        }
        let candidate = self
            .thumbnail_root
            .join(native_filename(event_id, attachment_id)?);
        let Ok(candidate) = candidate.canonicalize() else {
            return Ok(None);
        };
        Ok(candidate
            .starts_with(&self.thumbnail_root)
            .then_some(candidate))
    }

    fn stage_native_image(&self, filename: &str, bytes: &[u8]) -> anyhow::Result<bool> {
        self.stage_native_image_with_sync(filename, bytes, |directory| {
            #[cfg(unix)]
            {
                super::sync_directory(directory)
            }
            #[cfg(not(unix))]
            {
                let _ = directory;
                Ok(())
            }
        })
    }

    fn stage_native_image_with_sync(
        &self,
        filename: &str,
        bytes: &[u8],
        sync: impl FnOnce(&std::path::Path) -> std::io::Result<()>,
    ) -> anyhow::Result<bool> {
        let destination = self.thumbnail_root.join(filename);
        anyhow::ensure!(
            !destination.exists(),
            "native image identity is already committed"
        );
        let temporary = self
            .thumbnail_root
            .join(format!(".isapi-{}.tmp", uuid::Uuid::new_v4()));
        let mut linked = false;
        let result = (|| -> anyhow::Result<()> {
            crate::config::write_private_file(&temporary, bytes)?;
            fs::OpenOptions::new()
                .write(true)
                .open(&temporary)?
                .sync_all()?;
            fs::hard_link(&temporary, &destination)?;
            linked = true;
            sync(&self.thumbnail_root)?;
            Ok(())
        })();
        let _ = fs::remove_file(temporary);
        if result.is_err() && linked {
            fs::remove_file(&destination)?;
        }
        result?;
        Ok(true)
    }
}

fn validate_images(
    event: &TimelineEvent,
    images: &[(String, Arc<[u8]>)],
    maximum: u64,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        images.len() <= 16 && images.len() == event.attachments.len(),
        "native image descriptor count differs"
    );
    let total = images.iter().map(|(_, bytes)| bytes.len()).sum::<usize>();
    anyhow::ensure!(
        total <= 8 * 1024 * 1024 && (maximum == 0 || total as u64 <= maximum),
        "native images exceed storage capacity"
    );
    let mut identities = HashSet::new();
    for (id, bytes) in images {
        native_filename(&event.id, id)?;
        anyhow::ensure!(identities.insert(id), "duplicate native image identity");
        let descriptor = event
            .attachments
            .iter()
            .find(|descriptor| &descriptor.id == id)
            .ok_or_else(|| anyhow::anyhow!("native image descriptor missing"))?;
        anyhow::ensure!(
            descriptor.attachment_type == "snapshot"
                && descriptor.content_type == "image/jpeg"
                && descriptor.byte_len == Some(bytes.len() as u64)
                && bytes.len() <= 1024 * 1024,
            "native image descriptor differs from bytes"
        );
        validate_published_jpeg(bytes)?;
    }
    anyhow::ensure!(
        images.is_empty() || event.canonical_attachment().is_some(),
        "native images require a canonical descriptor"
    );
    Ok(())
}

fn native_filename(event_id: &str, attachment_id: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        safe_event_id(event_id)
            && safe_event_id(attachment_id)
            && event_id.len() <= 128
            && attachment_id.len() <= 96
            && attachment_id.starts_with("isapi-"),
        "invalid native image identity"
    );
    let filename = format!("{event_id}--{attachment_id}.jpg");
    anyhow::ensure!(
        safe_image_filename(&filename),
        "invalid native image filename"
    );
    Ok(filename)
}

#[cfg(test)]
mod tests {
    use super::EventStore;
    use crate::storage::RecordingCatalog;

    #[test]
    fn native_image_sync_failure_removes_only_its_uncommitted_destination() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-native-image-failure-{}",
            uuid::Uuid::new_v4()
        ));
        let catalog = RecordingCatalog::open(&directory.join("recordings.db")).unwrap();
        let root = directory.join("images");
        let store = EventStore::new(catalog.handle(), &root, 0).unwrap();
        let result =
            store.stage_native_image_with_sync("event--isapi-test.jpg", b"fixture", |_| {
                Err(std::io::Error::other("injected sync failure"))
            });
        assert!(result.is_err());
        assert!(!root.join("event--isapi-test.jpg").exists());
        store
            .stage_native_image("event--isapi-test.jpg", b"fixture")
            .unwrap();
        assert_eq!(
            std::fs::read(root.join("event--isapi-test.jpg")).unwrap(),
            b"fixture"
        );
        drop(store);
        catalog.shutdown();
        std::fs::remove_dir_all(directory).unwrap();
    }
}
