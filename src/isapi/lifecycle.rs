use std::collections::BTreeMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use crate::keeppeek::KeepPeekEvent;
use crate::storage::metadata::{EventSource, TimelineEvent, event_icon};
use ::isapi::{Event, Object, Target};

const ACTIVE_COUNT_MAX: usize = 64;
const OBSERVATION_TIMEOUT: Duration = Duration::from_secs(30);

mod images;
pub use images::queued_bytes;

#[derive(Clone)]
struct Active {
    id: String,
    start_time_ms: i64,
    last_time_ms: i64,
    last_received: Instant,
}

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
struct Key {
    rule: &'static str,
    kind: &'static str,
    region: Option<String>,
    object: Option<String>,
}

pub struct Outcome {
    pub changes: Vec<KeepPeekEvent>,
    pub activity: bool,
}

#[derive(Clone)]
pub struct Tracker {
    camera_ip: IpAddr,
    channel: u32,
    record_generic_motion: bool,
    active: BTreeMap<Key, Active>,
}

impl Tracker {
    pub const fn new(camera_ip: IpAddr, channel: u32, record_generic_motion: bool) -> Self {
        Self {
            camera_ip,
            channel,
            record_generic_motion,
            active: BTreeMap::new(),
        }
    }

    pub fn apply(
        &mut self,
        event: &Event,
        received: Instant,
        time_ms: i64,
    ) -> anyhow::Result<Outcome> {
        self.apply_images(event, &[], received, time_ms)
    }

    pub fn apply_bundle(
        &mut self,
        bundle: &::isapi::Bundle,
        received: Instant,
        time_ms: i64,
    ) -> anyhow::Result<Outcome> {
        if bundle.images().is_empty() {
            return self.apply(bundle.event(), received, time_ms);
        }
        self.apply_images(bundle.event(), bundle.images(), received, time_ms)
    }

    fn apply_images(
        &mut self,
        event: &Event,
        images: &[::isapi::Image],
        received: Instant,
        time_ms: i64,
    ) -> anyhow::Result<Outcome> {
        let mut outcome = Outcome {
            changes: Vec::new(),
            activity: false,
        };
        if event.is_heartbeat()
            || event.dynamic_channel_id().or_else(|| event.channel_id()) != Some(self.channel)
        {
            return Ok(outcome);
        }
        let Some(rule) = rule_kind(event.event_type()) else {
            return Ok(outcome);
        };
        if event.active() == Some(false) {
            self.clear(event, rule, time_ms, &mut outcome.changes);
            return Ok(outcome);
        }
        if event.active() != Some(true) {
            return Ok(outcome);
        }
        let objects: Vec<_> = if event.objects().is_empty() {
            vec![None]
        } else {
            event.objects().iter().map(Some).collect()
        };
        let observations: Vec<_> = objects
            .into_iter()
            .map(|object| (key(rule, event, object), object))
            .filter(|(key, _)| key.kind != "motion" || self.record_generic_motion)
            .collect();
        let new_count = observations
            .iter()
            .filter(|(key, _)| !self.active.contains_key(key))
            .count();
        let image_bytes = images.iter().map(|image| image.body().len()).sum::<usize>();
        anyhow::ensure!(
            image_bytes.saturating_mul(observations.len()) <= 16 * 1024 * 1024,
            "ISAPI observation image batch exceeds capacity"
        );
        anyhow::ensure!(
            new_count <= ACTIVE_COUNT_MAX.saturating_sub(self.active.len()),
            "ISAPI active observation limit exceeded"
        );
        outcome.activity = !observations.is_empty();
        let prepared = observations
            .into_iter()
            .map(|(key, object)| {
                let mut timeline = timeline_event(self.camera_ip, key.kind, event, object, time_ms);
                if let Some(active) = self.active.get(&key) {
                    timeline.id.clone_from(&active.id);
                    timeline.start_time_ms = active.start_time_ms;
                }
                let change = images::enrich(timeline, object, images)?;
                Ok((key, change))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        for (key, change) in prepared {
            if let Some(active) = self.active.get_mut(&key) {
                active.last_received = received;
                active.last_time_ms = time_ms.max(active.last_time_ms);
                if !images.is_empty() {
                    outcome.changes.push(change);
                }
            } else {
                let id = match &change {
                    KeepPeekEvent::TimelineEventStarted { event }
                    | KeepPeekEvent::TimelineEventImages { event, .. } => event.id.clone(),
                    _ => unreachable!("prepared observations are starts or image revisions"),
                };
                self.active.insert(
                    key,
                    Active {
                        id,
                        start_time_ms: time_ms,
                        last_time_ms: time_ms,
                        last_received: received,
                    },
                );
                outcome.changes.push(change);
            }
        }
        Ok(outcome)
    }

    fn clear(&mut self, event: &Event, rule: &str, time_ms: i64, changes: &mut Vec<KeepPeekEvent>) {
        self.active.retain(|key, active| {
            let matches = key.rule == rule
                && if event.objects().is_empty() {
                    classified_kind(event.detection_target()).is_none_or(|kind| kind == key.kind)
                } else {
                    event.objects().iter().any(|object| {
                        object
                            .id()
                            .is_none_or(|id| key.object.as_deref() == Some(id))
                            && object
                                .region()
                                .is_none_or(|region| key.region.as_deref() == Some(region))
                            && object
                                .target()
                                .and_then(target_kind)
                                .is_none_or(|kind| kind == key.kind)
                    })
                };
            if matches {
                changes.push(ended(active, time_ms.max(active.last_time_ms)));
            }
            !matches
        });
    }

    pub fn expire(&mut self, now: Instant) -> Vec<KeepPeekEvent> {
        let mut changes = Vec::with_capacity(self.active.len());
        self.active.retain(|_, active| {
            if now.saturating_duration_since(active.last_received) >= OBSERVATION_TIMEOUT {
                changes.push(ended(active, active.last_time_ms));
                return false;
            }
            true
        });
        changes
    }

    pub fn disconnect(&mut self) -> Vec<KeepPeekEvent> {
        std::mem::take(&mut self.active)
            .into_values()
            .map(|active| ended(&active, active.last_time_ms))
            .collect()
    }
}

fn ended(active: &Active, time_ms: i64) -> KeepPeekEvent {
    KeepPeekEvent::TimelineEventEnded {
        id: active.id.clone(),
        end_time_ms: time_ms,
    }
}

fn rule_kind(event_type: &str) -> Option<&'static str> {
    match event_type.to_ascii_lowercase().as_str() {
        "vmd" => Some("motion"),
        "linedetection" | "linecrossing" => Some("line_crossing"),
        "fielddetection" | "intrusion" => Some("intrusion"),
        "regionentrance" => Some("region_entry"),
        "regionexiting" => Some("region_exit"),
        "tamperdetection" => Some("tamper"),
        "io" => Some("alarm"),
        "targetcapture" => Some("motion"),
        "humanrecognition" | "humandetection" => Some("person"),
        "vehicledetection" | "anpr" => Some("vehicle"),
        _ => None,
    }
}

fn classified_kind(target: Option<&str>) -> Option<&'static str> {
    match target?.to_ascii_lowercase().as_str() {
        "human" | "person" => Some("person"),
        "vehicle" => Some("vehicle"),
        _ => None,
    }
}

const fn target_kind(target: Target) -> Option<&'static str> {
    match target {
        Target::Person => Some("person"),
        Target::Vehicle => Some("vehicle"),
        _ => None,
    }
}

fn key(rule: &'static str, event: &Event, object: Option<&Object>) -> Key {
    Key {
        rule,
        kind: object
            .and_then(Object::target)
            .and_then(target_kind)
            .or_else(|| classified_kind(event.detection_target()))
            .unwrap_or(rule),
        region: object.and_then(Object::region).map(str::to_owned),
        object: object
            .and_then(Object::id)
            .or_else(|| event.id())
            .map(str::to_owned),
    }
}

fn timeline_event(
    camera_ip: IpAddr,
    kind: &str,
    event: &Event,
    object: Option<&Object>,
    time_ms: i64,
) -> TimelineEvent {
    let payload = serde_json::json!({
        "protocol": "isapi", "event_type": event.event_type(),
        "channel_id": event.channel_id(), "dynamic_channel_id": event.dynamic_channel_id(),
        "camera_time": event.date_time(), "active_post_count": event.active_post_count(),
        "detection_target": event.detection_target(), "timestamp_basis": "server_received",
        "interval_semantics": "observed_activity",
        "end_time_semantics": "last_evidence_or_explicit_clear_not_proof_of_physical_stop",
        "camera_event_id": event.id(), "object": object,
    });
    TimelineEvent {
        id: uuid::Uuid::new_v4().to_string(),
        revision: 1,
        camera_id: camera_ip.to_string(),
        stream: None,
        source: EventSource::Camera,
        kind: kind.to_owned(),
        start_time_ms: time_ms,
        end_time_ms: None,
        confidence: object
            .and_then(Object::confidence)
            .map(::isapi::Confidence::normalized),
        bbox: None,
        bbox_attachment_id: None,
        zone: object.and_then(Object::region).map(str::to_owned),
        text: object.and_then(Object::plate).map(str::to_owned),
        payload: payload.as_object().cloned(),
        attachments: Vec::new(),
        canonical_attachment_id: None,
        icon_key: event_icon(None, kind).key.to_owned(),
        rejected_icon_key: None,
        thumbnail_filename: None,
    }
}

#[cfg(test)]
mod tests;
