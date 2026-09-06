use std::sync::Arc;

use crate::keeppeek::KeepPeekEvent;
use crate::storage::metadata::{EventAttachment, TimelineEvent};

pub fn enrich(
    mut timeline: TimelineEvent,
    object: Option<&::isapi::Object>,
    images: &[::isapi::Image],
) -> anyhow::Result<KeepPeekEvent> {
    let mut bytes = Vec::with_capacity(images.len());
    let mut references = serde_json::Map::new();
    for (ordinal, image) in images.iter().enumerate() {
        let id = format!("isapi-{}", uuid::Uuid::new_v4());
        references.insert(image.id().to_owned(), serde_json::Value::String(id.clone()));
        if object.and_then(::isapi::Object::image_id) == Some(image.id()) {
            if let Some(bbox) = object.and_then(::isapi::Object::bbox) {
                let dimensions = crate::storage::events::jpeg_dimensions(image.body())?;
                timeline.bbox = Some(bbox.normalized(Some(dimensions))?.map(|value| value as f32));
                timeline.bbox_attachment_id = Some(id.clone());
            }
            timeline.canonical_attachment_id = Some(id.clone());
        }
        timeline.attachments.push(EventAttachment {
            id: id.clone(),
            attachment_type: "snapshot".to_owned(),
            content_type: "image/jpeg".to_owned(),
            byte_len: Some(image.body().len() as u64),
            ordinal: u32::try_from(ordinal)?,
            timestamp_ms: Some(timeline.start_time_ms),
            text: None,
        });
        bytes.push((id, image.bytes()));
    }
    if timeline.canonical_attachment_id.is_none() {
        timeline.canonical_attachment_id = timeline
            .attachments
            .first()
            .map(|descriptor| descriptor.id.clone());
    }
    if !references.is_empty() {
        timeline.payload.get_or_insert_default().insert(
            "image_references".to_owned(),
            serde_json::Value::Object(references),
        );
    }
    anyhow::ensure!(
        serde_json::to_vec(&timeline.payload)?.len() <= 16 * 1024,
        "ISAPI event metadata exceeds storage contract"
    );
    Ok(if bytes.is_empty() {
        KeepPeekEvent::TimelineEventStarted {
            event: Box::new(timeline),
        }
    } else {
        KeepPeekEvent::TimelineEventImages {
            event: Box::new(timeline),
            images: bytes,
        }
    })
}

pub fn queued_bytes(event: &KeepPeekEvent) -> usize {
    match event {
        KeepPeekEvent::TimelineEventImages { images, .. } => images
            .iter()
            .map(|(_, bytes): &(String, Arc<[u8]>)| bytes.len())
            .sum(),
        _ => 0,
    }
}
