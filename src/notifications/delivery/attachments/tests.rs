use super::*;
use crate::storage::{
    RecordingCatalog, StorageConfig,
    catalog::locations::{Reply, Request},
    metadata::{EventAttachment, EventSource, TimelineEvent},
    volumes::{
        PlacementRequest, VolumeId, VolumeRole,
        runtime::{Manager, tests::fixture},
    },
};
use std::{io::Cursor, sync::Arc};

fn event(bytes: &[u8]) -> TimelineEvent {
    TimelineEvent {
        id: "notification-image".into(),
        revision: 1,
        camera_id: "camera".into(),
        stream: Some("main".into()),
        source: EventSource::KeepPeek,
        kind: "person".into(),
        start_time_ms: 1000,
        end_time_ms: None,
        confidence: None,
        bbox: None,
        bbox_attachment_id: None,
        zone: None,
        text: None,
        payload: None,
        attachments: vec![EventAttachment {
            id: "snapshot".into(),
            attachment_type: "snapshot".into(),
            content_type: "image/jpeg".into(),
            byte_len: Some(bytes.len() as u64),
            ordinal: 0,
            timestamp_ms: Some(1000),
            text: None,
        }],
        canonical_attachment_id: Some("snapshot".into()),
        icon_key: "person".into(),
        rejected_icon_key: None,
        thumbnail_filename: None,
    }
}

fn setup() -> anyhow::Result<(RecordingCatalog, Manager, EventStore, Delivery, Vec<u8>)> {
    let (root, catalog, initial) = fixture(1024 * 1024)?;
    let mut configuration = initial.configuration().clone();
    configuration.volumes[0].roles.push(VolumeRole::Thumbnail);
    let mut policy = configuration.placement[0].clone();
    policy.role = VolumeRole::Thumbnail;
    configuration.placement.push(policy);
    let manager = Manager::new(configuration, catalog.handle())?;
    let storage = StorageConfig {
        volume_runtime: Some(Arc::new(manager.clone())),
        ..StorageConfig::default()
    };
    let events =
        EventStore::new(catalog.handle(), &root.join("legacy"), 0)?.with_volume_storage(&storage);
    let mut jpeg = Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(16, 16).write_to(&mut jpeg, image::ImageFormat::Jpeg)?;
    let jpeg = jpeg.into_inner();
    events.commit_published_image("notification-publication", event(&jpeg), &jpeg)?;
    let current = events.event_by_id("notification-image")?.unwrap();
    let mut delivery = super::super::tests::pushover_delivery();
    delivery.attachment_enabled = true;
    delivery.attachment_required = true;
    delivery.attachment_path = Some(
        events
            .thumbnail_path("camera", &current.id)?
            .unwrap()
            .to_string_lossy()
            .into_owned(),
    );
    delivery.payload_json =
        serde_json::json!({ "title":"event", "body":"body", "deep_link":"/events/event",
        "event_id":current.id, "event_revision":current.revision,
        "canonical_attachment":current.canonical_attachment() })
        .to_string();
    Ok((catalog, manager, events, delivery, jpeg))
}

struct Check<'a> {
    events: &'a EventStore,
    catalog: &'a RecordingCatalog,
    expected: Result<Option<Vec<u8>>, &'static str>,
    leased: bool,
}

impl Provider for Check<'_> {
    fn deliver(&self, delivery: &Delivery) -> ProviderResult {
        if self.leased {
            let Reply::Location(Some(location)) = self
                .catalog
                .handle()
                .volume_location(Request::Image {
                    event: "notification-image".into(),
                    attachment: "snapshot".into(),
                })
                .unwrap()
            else {
                panic!("named image missing")
            };
            let path = self
                .events
                .thumbnail_path("camera", "notification-image")
                .unwrap()
                .unwrap();
            assert!(
                self.catalog
                    .handle()
                    .reader_leases()
                    .conflicts(&location.object.id, &path.to_string_lossy())
                    .unwrap()
            );
        }
        assert_eq!(super::super::provider_attachment(delivery), self.expected);
        super::super::DeliveryOutcome::Delivered {
            provider_status: None,
        }
        .into()
    }
}

#[test]
fn delivery_resolves_moved_image_and_holds_reader_while_provider_reads() -> anyhow::Result<()> {
    let (catalog, initial, events, delivery, jpeg) = setup()?;
    let (_, secondary_catalog, secondary_manager) = fixture(1024 * 1024)?;
    let mut configuration = initial.configuration().clone();
    let mut secondary = secondary_manager.configuration().volumes[0].clone();
    secondary.id = VolumeId::parse("secondary")?;
    secondary.roles.push(VolumeRole::Thumbnail);
    configuration.volumes.push(secondary);
    configuration.placement.last_mut().unwrap().candidates = vec![VolumeId::parse("secondary")?];
    let manager = Manager::new(configuration, catalog.handle())?;
    let storage = StorageConfig {
        volume_runtime: Some(Arc::new(manager.clone())),
        ..StorageConfig::default()
    };
    let events = events.with_volume_storage(&storage);
    let Reply::Location(Some(source)) = catalog.handle().volume_location(Request::Image {
        event: "notification-image".into(),
        attachment: "snapshot".into(),
    })?
    else {
        anyhow::bail!("image missing")
    };
    let move_id = uuid::Uuid::new_v4().to_string();
    assert!(manager.move_object(
        &move_id,
        source.object,
        &PlacementRequest {
            role: VolumeRole::Thumbnail,
            source: "camera",
            group: "",
            required_bytes: source.bytes,
        },
        &[],
        || false
    )?);
    assert!(manager.retire_move(&move_id)?);
    assert!(!std::path::Path::new(delivery.attachment_path.as_ref().unwrap()).exists());
    let provider = Check {
        events: &events,
        catalog: &catalog,
        expected: Ok(Some(jpeg)),
        leased: true,
    };
    deliver(&provider, &delivery, Some(&events));
    drop(events);
    drop(storage);
    drop(manager);
    drop(initial);
    drop(secondary_manager);
    secondary_catalog.shutdown();
    catalog.shutdown();
    Ok(())
}

#[test]
fn replaced_event_refuses_cached_image_and_preserves_optional_attachment_behavior()
-> anyhow::Result<()> {
    let (catalog, manager, events, mut delivery, jpeg) = setup()?;
    let mut replacement = event(&jpeg);
    replacement.revision = 2;
    events.commit_published_image("notification-replacement", replacement, &jpeg)?;
    let provider = Check {
        events: &events,
        catalog: &catalog,
        expected: Err("attachment_unavailable"),
        leased: false,
    };
    deliver(&provider, &delivery, Some(&events));
    delivery.attachment_required = false;
    let provider = Check {
        expected: Ok(None),
        ..provider
    };
    deliver(&provider, &delivery, Some(&events));
    drop(events);
    drop(manager);
    catalog.shutdown();
    Ok(())
}
