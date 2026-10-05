//! Places event images and resolves their current catalog-owned locations.

use super::EventStore;
use crate::storage::{
    catalog::{
        EventPublicationIdentity,
        locations::{Kind, Object, Reply, Request, images},
    },
    metadata::{EventAttachment, TimelineEvent},
    volumes::{VolumeRole, runtime::ReservedFile},
};
use std::io::Write;

impl EventStore {
    pub(super) fn legacy_image_references(
        &self,
        previous: Option<TimelineEvent>,
    ) -> anyhow::Result<Option<TimelineEvent>> {
        let Some(mut previous) = previous else {
            return Ok(None);
        };
        let mut legacy = Vec::with_capacity(previous.attachments.len());
        // ponytail: use existing location ownership to filter legacy cleanup references.
        for descriptor in previous.attachments {
            let Reply::Location(location) = self.catalog.volume_location(Request::Image {
                event: previous.id.clone(),
                attachment: descriptor.id.clone(),
            })?
            else {
                anyhow::bail!("invalid image location reply");
            };
            if location.is_none() {
                legacy.push(descriptor);
            } else if previous.canonical_attachment_id.as_deref() == Some(&descriptor.id) {
                previous.thumbnail_filename = None;
            }
        }
        previous.attachments = legacy;
        Ok(Some(previous))
    }

    pub(crate) fn leased_attachment_path(
        &self,
        event: &TimelineEvent,
        attachment: &str,
    ) -> anyhow::Result<
        Option<(
            std::path::PathBuf,
            Option<crate::storage::catalog::readers::LeaseSet>,
        )>,
    > {
        if let Some((location, lease)) = self.catalog.leased_event_image(event, attachment)? {
            let manager = self
                .volume_storage
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("image volume runtime unavailable"))?;
            return Ok(Some((manager.owned_path(&location)?, Some(lease))));
        }
        let Some(path) = self.attachment_path(&event.camera_id, &event.id, attachment)? else {
            return Ok(None);
        };
        let lease = self.catalog.lease_legacy_image(event, attachment, &path)?;
        Ok(Some((path, Some(lease))))
    }

    pub(super) fn commit_named_images(
        &self,
        mut event: TimelineEvent,
        publication: Option<EventPublicationIdentity>,
        images: &[(String, &[u8])],
    ) -> anyhow::Result<bool> {
        let Some(manager) = &self.volume_storage else {
            return Ok(false);
        };
        anyhow::ensure!(
            (1..=16).contains(&images.len()),
            "invalid event image count"
        );
        let mut writers = Vec::with_capacity(images.len());
        let result = (|| {
            let Some(publications) = self.stage_named_images(&mut event, images, &mut writers)?
            else {
                return Ok(false);
            };
            self.publish_named_images(
                images::Commit {
                    event,
                    publication,
                    images: publications,
                },
                &writers,
            )?;
            Ok(true)
        })();
        if result.is_err() {
            self.abandon_named_images(manager, &mut writers);
        }
        if let Some(worker) = &self.volume_mover
            && let Err(error) = worker.scan()
        {
            tracing::warn!(%error, "image cleanup wakeup failed; catalog retains pending work");
        }
        result
    }

    fn stage_named_images(
        &self,
        event: &mut TimelineEvent,
        images: &[(String, &[u8])],
        writers: &mut Vec<ReservedFile>,
    ) -> anyhow::Result<Option<Vec<images::Image>>> {
        let manager = self
            .volume_storage
            .as_ref()
            .expect("runtime checked by caller");
        let groups = self
            .volume_groups
            .read()
            .map_err(|_| anyhow::anyhow!("volume group registry unavailable"))?
            .get(&event.camera_id)
            .cloned()
            .unwrap_or_default();
        let groups = groups.iter().map(String::as_str).collect::<Vec<_>>();
        let mut publications = Vec::with_capacity(images.len());
        for (attachment_id, bytes) in images {
            let object_id = uuid::Uuid::new_v4().to_string();
            let object = Object {
                kind: Kind::Thumbnail,
                id: object_id.clone(),
            };
            let Some(reservation) = manager.reserve(
                VolumeRole::Thumbnail,
                &event.camera_id,
                &groups,
                object,
                bytes.len() as u64,
            )?
            else {
                anyhow::ensure!(
                    writers.is_empty(),
                    "image policy changed during publication"
                );
                return Ok(None);
            };
            let mut writer = reservation.open()?;
            writer.write_all(bytes)?;
            let evidence = writer.seal_image()?;
            if event.canonical_attachment_id.as_deref() == Some(attachment_id) {
                event.thumbnail_filename = Some(format!("{object_id}.jpg"));
            }
            publications.push(images::Image {
                attachment_id: attachment_id.clone(),
                object_id,
                evidence,
            });
            writers.push(writer);
        }
        Ok(Some(publications))
    }

    fn abandon_named_images(
        &self,
        manager: &crate::storage::volumes::runtime::Manager,
        writers: &mut Vec<ReservedFile>,
    ) {
        let mut operations = Vec::with_capacity(writers.len());
        for writer in writers.iter_mut() {
            let result = writer.evidence().and_then(|evidence| {
                let operation = evidence.operation.clone();
                self.catalog
                    .volume_location(Request::ImageAbandoned(evidence))?;
                operations.push(operation);
                Ok(())
            });
            if let Err(error) = result {
                tracing::warn!(%error, "unable to journal abandoned image; reservation preserved");
            }
        }
        writers.clear();
        for operation in operations {
            if let Err(error) = manager.retire_unused_image(&operation) {
                tracing::warn!(%error, "abandoned image cleanup deferred");
            }
        }
    }

    fn publish_named_images(
        &self,
        commit: images::Commit,
        writers: &[ReservedFile],
    ) -> anyhow::Result<()> {
        let request = Request::CommitImages(Box::new(commit));
        // Keep opened files pinned until the attachment and location transaction completes.
        for writer in writers {
            writer.validate_sealed_image()?;
        }
        let reply = match self.catalog.volume_location(request.clone()) {
            Ok(reply) => reply,
            Err(_) => {
                for writer in writers {
                    writer.validate_sealed_image()?;
                }
                self.catalog.volume_location(request)?
            }
        };
        anyhow::ensure!(
            matches!(reply, Reply::Bound),
            "invalid image publication reply"
        );
        Ok(())
    }

    pub(super) fn commit_named_snapshot(
        &self,
        mut event: TimelineEvent,
        bytes: &[u8],
    ) -> anyhow::Result<bool> {
        let attachment_id = event
            .canonical_attachment_id
            .clone()
            .unwrap_or_else(|| "thumbnail".into());
        let descriptor = EventAttachment {
            id: attachment_id.clone(),
            attachment_type: "thumbnail".into(),
            content_type: "image/jpeg".into(),
            byte_len: Some(bytes.len() as u64),
            ordinal: 0,
            timestamp_ms: Some(event.start_time_ms),
            text: None,
        };
        if let Some(existing) = event
            .attachments
            .iter_mut()
            .find(|item| item.id == attachment_id)
        {
            *existing = descriptor;
        } else {
            event.attachments.push(descriptor);
        }
        event.canonical_attachment_id = Some(attachment_id.clone());
        event.revision = event
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("event revision exhausted"))?;
        self.commit_named_images(event, None, &[(attachment_id, bytes)])
    }

    pub(super) fn named_image_path(
        &self,
        event: &str,
        attachment: &str,
    ) -> anyhow::Result<Option<std::path::PathBuf>> {
        let Reply::Location(location) = self.catalog.volume_location(Request::Image {
            event: event.to_owned(),
            attachment: attachment.to_owned(),
        })?
        else {
            anyhow::bail!("invalid image location reply")
        };
        let Some(location) = location else {
            return Ok(None);
        };
        let manager = self
            .volume_storage
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("image volume runtime unavailable"))?;
        Ok(Some(manager.owned_path(&location)?))
    }
}
