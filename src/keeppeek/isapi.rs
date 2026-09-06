use super::{KeepPeekEvent, KeepPeekLoop, NotificationStage, Trigger, unix_time_ms};

impl KeepPeekLoop {
    pub(crate) fn configure_isapi_callbacks(
        &mut self,
        config: Option<&crate::isapi::callbacks::Config>,
        cameras: &std::collections::HashMap<std::net::IpAddr, crate::cameras::Camera>,
    ) -> anyhow::Result<()> {
        let Some(config) = config else {
            return Ok(());
        };
        anyhow::ensure!(
            self.events.is_some(),
            "ISAPI callbacks require event storage"
        );
        for source in &config.sources {
            let camera = cameras.get(&source.ip).ok_or_else(|| {
                anyhow::anyhow!("ISAPI callback source is not a configured camera")
            })?;
            anyhow::ensure!(
                !camera.is_reolink,
                "ISAPI callbacks cannot replace a Reolink event source"
            );
        }
        self.isapi_callbacks = Some(crate::isapi::callbacks::Runtime::start(
            config,
            self.tx.clone(),
            self.shutdown.clone(),
        )?);
        Ok(())
    }
    pub(super) fn commit_isapi_change(&self, change: KeepPeekEvent) -> anyhow::Result<()> {
        let events = self
            .events
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("ISAPI callback event storage is unavailable"))?;
        let (event, trigger) = match change {
            KeepPeekEvent::TimelineEventStarted { event } => {
                events.insert((*event).clone())?;
                (*event, Trigger::EventCreated)
            }
            KeepPeekEvent::TimelineEventImages { event, images } => {
                let event = events.commit_native_images(*event, &images)?;
                let trigger = if event.revision == 1 {
                    Trigger::EventCreated
                } else {
                    Trigger::EventUpdated
                };
                (event, trigger)
            }
            KeepPeekEvent::TimelineEventThumbnail {
                camera_id,
                event_id,
                jpeg,
            } => {
                events.save_thumbnail(&camera_id, &event_id, &jpeg)?;
                let event = events.event_by_id(&event_id)?.ok_or_else(|| {
                    anyhow::anyhow!("native event disappeared after thumbnail commit")
                })?;
                (event, Trigger::EventUpdated)
            }
            KeepPeekEvent::TimelineEventEnded { id, end_time_ms } => {
                let Some(event) = events.close_native_event(&id, end_time_ms)? else {
                    return Ok(());
                };
                (event, Trigger::EventEnded)
            }
            _ => anyhow::bail!("invalid ISAPI callback transition"),
        };
        if let Some(storage) = &self.storage {
            storage.note_camera_event(&event.camera_id);
        }
        let image = event.thumbnail_filename.as_ref().and_then(|_| {
            events
                .thumbnail_path(&event.camera_id, &event.id)
                .ok()
                .flatten()
        });
        self.publish_event_revision(
            &event,
            trigger,
            NotificationStage::Enriched,
            image.as_deref(),
            unix_time_ms(),
        );
        Ok(())
    }
}
