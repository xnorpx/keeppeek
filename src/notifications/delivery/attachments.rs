use super::{Delivery, Provider, ProviderPayload, ProviderResult};
use crate::storage::{EventStore, catalog::readers::LeaseSet};

pub(super) fn deliver(
    provider: &dyn Provider,
    delivery: &Delivery,
    events: Option<&EventStore>,
) -> ProviderResult {
    let Some(events) = events.filter(|_| delivery.attachment_enabled) else {
        return provider.deliver(delivery);
    };
    let mut resolved = delivery.clone();
    resolved.attachment_path = None;
    let attachment = resolve(events, delivery);
    if let Some((path, _)) = &attachment {
        resolved.attachment_path = Some(path.to_string_lossy().into_owned());
    }
    // The lease must survive the provider's bounded attachment read.
    let result = provider.deliver(&resolved);
    drop(attachment);
    result
}

fn resolve(
    events: &EventStore,
    delivery: &Delivery,
) -> Option<(std::path::PathBuf, Option<LeaseSet>)> {
    let payload: ProviderPayload = serde_json::from_str(&delivery.payload_json).ok()?;
    let event = events.event_by_id(payload.event_id.as_deref()?).ok()??;
    let descriptor = payload.canonical_attachment.as_ref()?;
    if payload.event_revision != Some(event.revision)
        || event.canonical_attachment() != Some(descriptor)
    {
        return None;
    }
    events.leased_attachment_path(&event, &descriptor.id).ok()?
}

#[cfg(test)]
mod tests;
