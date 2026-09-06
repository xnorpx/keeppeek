use std::io::Read;
use std::sync::Arc;

use super::{
    CameraInfo, EventSource, PUBLISHED_DETECTION_EVENT_TYPES, ServerState, TimelineEvent,
    camera_source_session_id, event_subscription, millis_timestamp, proto,
    proto_event_attachment_descriptor, proto_event_image_availability, proto_event_payload,
    published_snapshot_capability,
};

const NATIVE_EVENT_TYPES: [&str; 9] = [
    "person",
    "vehicle",
    "motion",
    "line_crossing",
    "intrusion",
    "region_entry",
    "region_exit",
    "tamper",
    "alarm",
];
const THUMBNAIL_SIZE_BYTES_MAX: u64 = 1024 * 1024;

impl ServerState {
    pub(crate) fn publish_camera_event(&self, event: &TimelineEvent) {
        if event.source != EventSource::Camera {
            return;
        }
        let Some(camera) = self
            .camera_entries()
            .into_iter()
            .find(|camera| camera.info.id == event.camera_id || camera.info.ip == event.camera_id)
        else {
            return;
        };
        let source_session =
            camera.info.ip.parse().ok().map(|ip| {
                camera_source_session_id(&camera.info.id, self.webrtc.camera_generation(ip))
            });
        let image = self
            .events
            .as_ref()
            .and_then(|store| {
                store
                    .thumbnail_path(&event.camera_id, &event.id)
                    .ok()
                    .flatten()
            })
            .and_then(|path| {
                let mut bytes = Vec::new();
                let file = std::fs::File::open(path).ok()?;
                file.take(THUMBNAIL_SIZE_BYTES_MAX + 1)
                    .read_to_end(&mut bytes)
                    .ok()?;
                (!bytes.is_empty() && bytes.len() as u64 <= THUMBNAIL_SIZE_BYTES_MAX)
                    .then(|| Arc::<[u8]>::from(bytes))
            });
        let mut message = message(event, source_session, image.is_some());
        message.source_id = camera.info.id.clone();
        let mut additional = Vec::new();
        let mut remaining = 8 * 1024 * 1024 - image.as_ref().map_or(0, |bytes| bytes.len());
        if let Some(store) = &self.events {
            for descriptor in event.attachments.iter().take(16).filter(|descriptor| {
                Some(descriptor.id.as_str()) != event.canonical_attachment_id.as_deref()
            }) {
                if let Some(bytes) = read_image(store, event, &descriptor.id, remaining) {
                    remaining -= bytes.len();
                    additional.push((descriptor.id.clone(), bytes));
                }
            }
        }
        event_subscription::publish_native_images(self, &camera.info, &message, image, &additional);
    }
}

fn read_image(
    store: &crate::storage::events::EventStore,
    event: &TimelineEvent,
    id: &str,
    remaining: usize,
) -> Option<Arc<[u8]>> {
    let descriptor = event
        .attachments
        .iter()
        .find(|descriptor| descriptor.id == id && descriptor.content_type == "image/jpeg")?;
    let path = store
        .attachment_path(&event.camera_id, &event.id, id)
        .ok()??;
    let limit = remaining.min(THUMBNAIL_SIZE_BYTES_MAX as usize);
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (!bytes.is_empty() && bytes.len() <= limit && descriptor.byte_len == Some(bytes.len() as u64))
        .then(|| bytes.into())
}

pub(super) fn reported_types(
    camera: &CameraInfo,
    registry: &crate::camera_events::Registry,
) -> Vec<proto::EventType> {
    if camera
        .ip
        .parse()
        .ok()
        .is_some_and(|ip| !registry.enabled(ip))
    {
        return Vec::new();
    }
    let Some(evidence) = camera.ip.parse().ok().and_then(|ip| registry.snapshot(ip)) else {
        return event_types(camera);
    };
    evidence
        .kinds
        .iter()
        .map(|kind| {
            let mut snapshot = published_snapshot_capability();
            snapshot.minimum_count = 0;
            snapshot.maximum_count = if evidence.mode == "vendor" && !camera.is_reolink {
                16
            } else {
                1
            };
            proto::EventType {
                event_type: kind.clone(),
                metadata: None,
                attachments: vec![snapshot],
            }
        })
        .collect()
}

pub(super) fn with_publication_types(mut kinds: Vec<proto::EventType>) -> Vec<proto::EventType> {
    for kind in PUBLISHED_DETECTION_EVENT_TYPES {
        if !kinds.iter().any(|existing| existing.event_type == kind) {
            kinds.push(proto::EventType {
                event_type: kind.to_owned(),
                metadata: None,
                attachments: vec![published_snapshot_capability()],
            });
        }
    }
    kinds
}

pub(super) fn event_types(camera: &CameraInfo) -> Vec<proto::EventType> {
    if !camera.capabilities.events {
        return Vec::new();
    }
    let native = camera.capabilities.events && !camera.is_reolink;
    let kinds: &[&str] = if native {
        &NATIVE_EVENT_TYPES
    } else {
        &PUBLISHED_DETECTION_EVENT_TYPES
    };
    kinds
        .iter()
        .map(|kind| {
            let mut snapshot = published_snapshot_capability();
            if native {
                snapshot.minimum_count = 0;
                snapshot.maximum_count = 16;
            }
            proto::EventType {
                event_type: (*kind).to_owned(),
                metadata: None,
                attachments: vec![snapshot],
            }
        })
        .collect()
}

pub(super) fn message(
    event: &TimelineEvent,
    source_session_id: Option<String>,
    image_available: bool,
) -> proto::Event {
    proto::Event {
        event_id: event.id.clone(),
        revision: event.revision,
        source_id: event.camera_id.clone(),
        media_kind: Some(proto::MediaKind::Video as i32),
        origin: proto::EventOrigin::Camera as i32,
        event_type: event.kind.clone(),
        start_time: Some(millis_timestamp(event.start_time_ms)),
        end_time: event.end_time_ms.map(millis_timestamp),
        confidence: event.confidence,
        bounding_box: event
            .bbox
            .map(|[x, y, width, height]| proto::EventBoundingBox {
                x,
                y,
                width,
                height,
            }),
        zone: event.zone.clone(),
        text: event.text.clone(),
        payload: event.payload.clone().map(proto_event_payload),
        attachments: event
            .attachments
            .iter()
            .cloned()
            .map(proto_event_attachment_descriptor)
            .collect(),
        source_session_id,
        subscription_id: None,
        canonical_attachment_id: event.canonical_attachment_id.clone(),
        icon_key: Some(event.icon_key.clone()),
        rejected_icon_key: event.rejected_icon_key.clone(),
        bounding_box_attachment_id: event.bbox_attachment_id.clone(),
        image_availability: proto_event_image_availability(
            event.canonical_attachment_id.is_some(),
            image_available,
        ),
    }
}
