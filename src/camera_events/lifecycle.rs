//! Tracks camera-native notification and metadata lifecycles without transport I/O.
//!
//! One owner must feed both PullPoint and EventStream notifications into this tracker.
//! Topic filters match complete namespace-expanded paths exactly. Exclusions win.
//! Initialized properties establish a baseline, not a replayable event opening.
//! An initialized active baseline requires a false or deleted transition before opening.
//! A disconnected event that actually started may reopen on a newer Changed observation.
//! Unbound object coordinates belong to the payload, never to an unattached event box.
//!
//! Limits per camera are 128 open or deferred events, 128 initialized baselines,
//! 1024 watermarks, 1024 replay fingerprints, and 16 KiB per serialized payload.
//! Capacity errors do not evict active events or partially apply a frame. History
//! evicts old unpinned identities, so replay protection has a bounded retention window.
//! Property deadlines are 30 seconds; object deadlines are five seconds. Deadlines
//! use the caller's Instant clock. An inferred ending never extends past observation.
//!
//! Camera UTC is accepted from the preceding 300 seconds, inclusive. Future times
//! and times outside that window use receipt time. The payload retains cameraTime
//! and timestamp_reason. Finish times never precede the event's start.
//!
//! Commit returned commands in order. Call expire at least once per second, including
//! while disconnected, to publish pending final details. The server owns revision
//! increments. Derive snapshot requests from TimelineEventStarted when policy.snapshots
//! is enabled; this tracker does not queue or fetch images. Only a matching attachment
//! can give payload.object_box an image association outside this module.

mod history;
mod objects;
mod observation;

#[cfg(test)]
mod source_tests;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::time::{Duration, Instant};

use onvif::event::{Frame, Kind, Notification, normalize};

use crate::cameras::events::{EventConfig, EventMode, MetadataMode};
use crate::keeppeek::KeepPeekEvent;
use crate::storage::metadata::{EventSource, TimelineEvent, event_icon};
use history::History;
use observation::{Action, Details, Identity, Observation};

/// Open events and deferred final revisions share this per-camera memory budget.
const ACTIVE_MAX: usize = 128;
/// Initialized properties must not grow with an unbounded number of camera rules.
const BASELINE_MAX: usize = 128;
/// Replay evidence is retained across transport reconnects, but not without a bound.
const HISTORY_MAX: usize = 1024;
/// Metadata updates must not cause a storage write for each analytics frame.
const UPDATE_INTERVAL: Duration = Duration::from_secs(1);
/// Properties expire when their transport stops providing fresh observations.
const PROPERTY_TIMEOUT: Duration = Duration::from_secs(30);
/// Partial analytics frames use a shorter disappearance deadline than properties.
const OBJECT_TIMEOUT: Duration = Duration::from_secs(5);

const _: () = assert!(
    ACTIVE_MAX + BASELINE_MAX < HISTORY_MAX,
    "active events and baselines must leave room for watermark eviction"
);

/// Owns one camera's native event state across all of its ONVIF transports.
pub(super) struct Tracker {
    camera_id: String,
    policy: EventConfig,
    record_motion: bool,
    active: HashMap<Identity, Active>,
    baselines: HashSet<Identity>,
    history: History,
    pending: Vec<Active>,
    deduplicated: u64,
}

struct Active {
    event: TimelineEvent,
    details: Details,
    published: Details,
    last_seen: Instant,
    last_time_ms: i64,
    last_published: Instant,
    last_camera_time: chrono::DateTime<chrono::Utc>,
    pullpoint: bool,
    metadata: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Transport {
    PullPoint,
    Metadata,
}

impl Tracker {
    /// Creates a camera owner. The caller keeps this owner across reconnects.
    pub(super) fn new(camera_id: String, policy: EventConfig, record_motion: bool) -> Self {
        Self {
            camera_id,
            policy,
            record_motion,
            active: HashMap::with_capacity(ACTIVE_MAX),
            baselines: HashSet::with_capacity(BASELINE_MAX),
            history: History::new(),
            pending: Vec::with_capacity(ACTIVE_MAX),
            deduplicated: 0,
        }
    }

    /// Applies one notification without emitting raw peer data in errors or diagnostics.
    ///
    /// # Errors
    /// Rejects invalid normalized input, oversized payloads, and exhausted state budgets.
    /// Errors leave lifecycle state unchanged.
    pub(super) fn apply(
        &mut self,
        notification: &Notification,
        received: Instant,
        received_ms: i64,
    ) -> anyhow::Result<Vec<KeepPeekEvent>> {
        self.apply_from(notification, received, received_ms, Transport::PullPoint)
    }

    /// Applies a notification carried by the camera-bound metadata stream.
    pub(super) fn apply_metadata(
        &mut self,
        notification: &Notification,
        received: Instant,
        received_ms: i64,
    ) -> anyhow::Result<Vec<KeepPeekEvent>> {
        if self.policy.metadata_stream == MetadataMode::Disabled {
            return Ok(Vec::new());
        }
        self.apply_from(notification, received, received_ms, Transport::Metadata)
    }

    fn apply_from(
        &mut self,
        notification: &Notification,
        received: Instant,
        received_ms: i64,
        transport: Transport,
    ) -> anyhow::Result<Vec<KeepPeekEvent>> {
        let Some(mut observation) =
            self.prepare_notification(notification, received, received_ms)?
        else {
            return Ok(Vec::new());
        };
        let active = observation.action == Action::Active;
        if self.history.is_replay(&observation) {
            if active {
                self.remember_transport(
                    &observation.key,
                    observation.clock.camera,
                    observation.kind,
                    transport,
                );
            }
            self.deduplicated = self.deduplicated.saturating_add(1);
            return Ok(Vec::new());
        }
        self.prepare_details(&mut observation)?;
        self.check_capacity(&observation)?;
        let key = observation.key.clone();
        let camera_time = observation.clock.camera;
        let kind = observation.kind;
        let mut changes = Vec::with_capacity(4);
        self.commit(observation, &mut changes);
        if active {
            self.remember_transport(&key, camera_time, kind, transport);
        }
        Ok(changes)
    }

    fn prepare_notification(
        &self,
        notification: &Notification,
        received: Instant,
        received_ms: i64,
    ) -> anyhow::Result<Option<Observation>> {
        self.validate_policy()?;
        if self.policy.mode == EventMode::Disabled || !observation::known_operation(notification) {
            return Ok(None);
        }
        let Some(detection) = normalize(notification)? else {
            return Ok(None);
        };
        if !self.policy.source_tokens.is_empty()
            && !self
                .policy
                .source_tokens
                .iter()
                .any(|token| detection.source.as_deref() == Some(token.as_str()))
        {
            return Ok(None);
        }
        let Some(mut observation) =
            observation::notification(notification, detection, received, received_ms)?
        else {
            return Ok(None);
        };
        if !self.accepts_topic(&observation.payload["topic"]) {
            return Ok(None);
        }
        observation.kind = observation
            .kind
            .map(|kind| self.retained_kind(&observation.key, kind));
        if observation.kind == Some(Kind::Motion)
            && !self.record_motion
            && !matches!(observation.action, Action::End | Action::Initialized(false))
        {
            return Ok(None);
        }
        Ok(Some(observation))
    }

    /// Applies a partial analytics frame from an already camera-bound metadata stream.
    ///
    /// Frame source names identify analytics modules, not camera source tokens.
    /// Notification topic/source filters do not reinterpret this separate identity.
    /// Unknown objects do not open events. Classless updates can update known objects,
    /// but their unclassified confidence cannot replace a known class confidence.
    ///
    /// # Errors
    /// Rejects invalid objects, ambiguous duplicate IDs, and exhausted state budgets.
    /// The whole frame is checked before any lifecycle state changes.
    pub(super) fn frame(
        &mut self,
        frame: &Frame,
        received: Instant,
        received_ms: i64,
    ) -> anyhow::Result<Vec<KeepPeekEvent>> {
        self.validate_policy()?;
        if self.policy.mode == EventMode::Disabled
            || self.policy.metadata_stream == MetadataMode::Disabled
        {
            return Ok(Vec::new());
        }
        let mut observations = objects::prepare(self, frame, received, received_ms)?;
        let mut duplicates = 0;
        observations.retain(|observation| {
            if self.history.is_replay(observation) {
                duplicates += 1;
                false
            } else {
                true
            }
        });
        for observation in &mut observations {
            self.prepare_details(observation)?;
        }
        objects::check_capacity(self, &observations)?;
        self.deduplicated = self.deduplicated.saturating_add(duplicates);
        let mut changes = Vec::with_capacity(observations.len().saturating_mul(3));
        for observation in observations {
            self.commit(observation, &mut changes);
        }
        Ok(changes)
    }

    /// Flushes due metadata revisions and closes stale intervals at their last observation.
    ///
    /// Call this while transports are disconnected too. A final throttled revision may
    /// follow its Ended command; native image commits preserve the stored end and images.
    pub(super) fn expire(&mut self, now: Instant) -> Vec<KeepPeekEvent> {
        let mut changes = Vec::with_capacity(self.active.len() + self.pending.len());
        self.pending.retain_mut(|active| {
            if let Some(update) = active.flush(now) {
                changes.push(update);
            }
            active.dirty()
        });
        let expired: Vec<_> = self
            .active
            .iter()
            .filter_map(|(key, active)| {
                let timeout = match key {
                    Identity::Property(_) => PROPERTY_TIMEOUT,
                    Identity::Object { .. } => OBJECT_TIMEOUT,
                };
                (now.saturating_duration_since(active.last_seen) >= timeout).then(|| key.clone())
            })
            .collect();
        for key in expired {
            self.close(&key, None, Some(now), &mut changes);
        }
        for active in self.active.values_mut() {
            if let Some(update) = active.flush(now) {
                changes.push(update);
            }
        }
        changes
    }

    /// Closes open intervals without clearing baselines or replay evidence.
    ///
    /// The caller owns reason diagnostics. Arbitrary reason text is not persisted.
    pub(super) fn disconnect(&mut self, _reason: &str) -> Vec<KeepPeekEvent> {
        let keys: Vec<_> = self.active.keys().cloned().collect();
        let mut changes = Vec::with_capacity(keys.len());
        for key in keys {
            self.close(&key, None, None, &mut changes);
        }
        changes
    }

    /// Closes metadata-only properties and objects while preserving PullPoint-backed events.
    pub(super) fn disconnect_metadata(&mut self, _reason: &str) -> Vec<KeepPeekEvent> {
        self.disconnect_transport(Transport::Metadata)
    }

    /// Closes PullPoint-only properties while preserving metadata-backed events.
    pub(super) fn disconnect_pullpoint(&mut self, _reason: &str) -> Vec<KeepPeekEvent> {
        self.disconnect_transport(Transport::PullPoint)
    }

    fn disconnect_transport(&mut self, transport: Transport) -> Vec<KeepPeekEvent> {
        let keys: Vec<_> = self
            .active
            .iter_mut()
            .filter_map(|(key, active)| {
                let close = match key {
                    Identity::Object { .. } => transport == Transport::Metadata,
                    Identity::Property(_) => {
                        match transport {
                            Transport::PullPoint => active.pullpoint = false,
                            Transport::Metadata => active.metadata = false,
                        }
                        !active.pullpoint && !active.metadata
                    }
                };
                close.then(|| key.clone())
            })
            .collect();
        let mut changes = Vec::with_capacity(keys.len());
        for key in keys {
            self.close(&key, None, None, &mut changes);
        }
        changes
    }

    fn remember_transport(
        &mut self,
        key: &Identity,
        camera_time: chrono::DateTime<chrono::Utc>,
        kind: Option<Kind>,
        transport: Transport,
    ) {
        if let Some(active) = self.active.get_mut(key)
            && active.last_camera_time == camera_time
            && Some(active.event.kind.as_str()) == kind.map(Kind::as_str)
        {
            match transport {
                Transport::PullPoint => active.pullpoint = true,
                Transport::Metadata => active.metadata = true,
            }
        }
    }

    /// Returns the sorted, unique kinds of currently open intervals.
    pub(super) fn kinds(&self) -> Vec<String> {
        let mut kinds: Vec<_> = self
            .active
            .values()
            .map(|active| active.event.kind.clone())
            .collect();
        kinds.sort_unstable();
        kinds.dedup();
        kinds
    }

    /// Counts open intervals, excluding baselines, point events, and deferred final updates.
    pub(super) fn active_count(&self) -> usize {
        self.active.len()
    }

    /// Counts rejected duplicate or stale observations with saturation at u64::MAX.
    pub(super) const fn deduplicated(&self) -> u64 {
        self.deduplicated
    }

    fn validate_policy(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.camera_id.len() <= 256,
            "camera event owner exceeds limit"
        );
        anyhow::ensure!(
            self.policy.source_tokens.len() <= 32,
            "event source filter count exceeds limit"
        );
        anyhow::ensure!(
            self.policy
                .include_topics
                .len()
                .saturating_add(self.policy.exclude_topics.len())
                <= 32,
            "event topic filter count exceeds limit"
        );
        Ok(())
    }

    fn accepts_topic(&self, topic: &serde_json::Value) -> bool {
        let Some(topic) = topic.as_str() else {
            return false;
        };
        !self
            .policy
            .exclude_topics
            .iter()
            .any(|filter| filter == topic)
            && (self.policy.include_topics.is_empty()
                || self
                    .policy
                    .include_topics
                    .iter()
                    .any(|filter| filter == topic))
    }

    fn retained_kind(&self, key: &Identity, kind: Kind) -> Kind {
        if matches!(kind, Kind::Motion | Kind::Intrusion | Kind::Loitering) {
            self.history.open_kind(key).unwrap_or(kind)
        } else {
            kind
        }
    }

    fn check_capacity(&self, observation: &Observation) -> anyhow::Result<()> {
        if observation.action == Action::Initialized(true)
            && !self.baselines.contains(&observation.key)
            && self.history.open_kind(&observation.key).is_none()
        {
            anyhow::ensure!(
                self.baselines.len() < BASELINE_MAX,
                "camera event baseline capacity reached"
            );
        }
        if observation.action == Action::Active && !self.baselines.contains(&observation.key) {
            anyhow::ensure!(
                !self.needs_slot(observation)
                    || self.active.len() + self.pending.len() < ACTIVE_MAX,
                "camera event active capacity reached"
            );
        }
        Ok(())
    }

    fn needs_slot(&self, observation: &Observation) -> bool {
        self.active.get(&observation.key).is_none_or(|active| {
            Some(active.event.kind.as_str()) != observation.kind.map(Kind::as_str)
                && active.deferred(observation.clock.received)
        })
    }

    fn prepare_details(&self, observation: &mut Observation) -> anyhow::Result<()> {
        if let Some(active) = self.active.get(&observation.key)
            && Some(active.event.kind.as_str()) == observation.kind.map(Kind::as_str)
        {
            let mut details = active.details.clone();
            details.merge(&observation.details);
            observation.details = details;
        }
        observation.payload.insert(
            "object_box".to_owned(),
            serde_json::json!(observation.details.bbox),
        );
        observation::validate_payload(&observation.payload)
    }

    fn commit(&mut self, observation: Observation, changes: &mut Vec<KeepPeekEvent>) {
        let was_open = self.history.open_kind(&observation.key).is_some();
        match observation.action {
            Action::Initialized(state) => {
                if state && !was_open {
                    self.baselines.insert(observation.key.clone());
                } else if !state {
                    self.baselines.remove(&observation.key);
                }
            }
            Action::End => {
                self.baselines.remove(&observation.key);
                self.close(
                    &observation.key,
                    Some(observation.clock.time_ms),
                    Some(observation.clock.received),
                    changes,
                );
            }
            Action::Active if !self.baselines.contains(&observation.key) => {
                self.upsert(&observation, changes);
            }
            Action::Point => {
                let kind = observation
                    .kind
                    .expect("point events require an explicit kind");
                let mut event = self.event(&observation, kind);
                event.end_time_ms = Some(event.start_time_ms);
                changes.push(KeepPeekEvent::TimelineEventStarted {
                    event: Box::new(event),
                });
            }
            Action::Active => {}
        }
        let opened = self.active.contains_key(&observation.key)
            || (observation.action == Action::Initialized(true) && was_open);
        self.history.remember(&observation, opened, |key| {
            self.active.contains_key(key) || self.baselines.contains(key)
        });
    }

    fn upsert(&mut self, observation: &Observation, changes: &mut Vec<KeepPeekEvent>) {
        let kind = observation
            .kind
            .expect("active events require an explicit kind");
        if self
            .active
            .get(&observation.key)
            .is_some_and(|active| active.event.kind != kind.as_str())
        {
            self.close(
                &observation.key,
                Some(observation.clock.time_ms),
                Some(observation.clock.received),
                changes,
            );
        }
        if let Some(active) = self.active.get_mut(&observation.key) {
            active.observe(observation);
            if let Some(update) = active.flush(observation.clock.received) {
                changes.push(update);
            }
        } else {
            let event = self.event(observation, kind);
            changes.push(KeepPeekEvent::TimelineEventStarted {
                event: Box::new(event.clone()),
            });
            self.active.insert(
                observation.key.clone(),
                Active {
                    event,
                    details: observation.details.clone(),
                    published: observation.details.clone(),
                    last_seen: observation.clock.received,
                    last_time_ms: observation.clock.time_ms,
                    last_published: observation.clock.received,
                    last_camera_time: observation.clock.camera,
                    pullpoint: false,
                    metadata: false,
                },
            );
        }
    }

    fn event(&self, observation: &Observation, kind: Kind) -> TimelineEvent {
        TimelineEvent {
            id: uuid::Uuid::new_v4().to_string(),
            revision: 1,
            camera_id: self.camera_id.clone(),
            stream: None,
            source: EventSource::Camera,
            kind: kind.as_str().to_owned(),
            start_time_ms: observation.clock.time_ms,
            end_time_ms: None,
            confidence: observation.details.confidence,
            bbox: None,
            bbox_attachment_id: None,
            zone: None,
            text: observation.details.text.clone(),
            payload: Some(observation.payload.clone()),
            attachments: Vec::new(),
            canonical_attachment_id: None,
            icon_key: event_icon(None, kind.as_str()).key.to_owned(),
            rejected_icon_key: None,
            thumbnail_filename: None,
        }
    }

    fn close(
        &mut self,
        key: &Identity,
        time_ms: Option<i64>,
        now: Option<Instant>,
        changes: &mut Vec<KeepPeekEvent>,
    ) {
        let Some(mut active) = self.active.remove(key) else {
            return;
        };
        if let Some(now) = now
            && let Some(update) = active.flush(now)
        {
            changes.push(update);
        }
        let end_time_ms = time_ms
            .unwrap_or(active.last_time_ms)
            .max(active.last_time_ms)
            .max(active.event.start_time_ms);
        changes.push(KeepPeekEvent::TimelineEventEnded {
            id: active.event.id.clone(),
            end_time_ms,
        });
        if active.dirty() {
            active.event.end_time_ms = Some(end_time_ms);
            self.pending.push(active);
        }
        assert!(
            self.active.len() + self.pending.len() <= ACTIVE_MAX,
            "camera event state exceeded its budget"
        );
    }
}

impl Active {
    fn observe(&mut self, observation: &Observation) {
        self.details.merge(&observation.details);
        self.event.confidence = self.details.confidence;
        self.event.text.clone_from(&self.details.text);
        let mut payload = observation.payload.clone();
        payload.insert(
            "object_box".to_owned(),
            serde_json::json!(self.details.bbox),
        );
        self.event.payload = Some(payload);
        self.last_seen = self.last_seen.max(observation.clock.received);
        self.last_time_ms = self.last_time_ms.max(observation.clock.time_ms);
        self.last_camera_time = observation.clock.camera;
    }

    fn dirty(&self) -> bool {
        self.details != self.published
    }

    fn deferred(&self, now: Instant) -> bool {
        self.dirty() && now.saturating_duration_since(self.last_published) < UPDATE_INTERVAL
    }

    fn flush(&mut self, now: Instant) -> Option<KeepPeekEvent> {
        if !self.dirty() || self.deferred(now) {
            return None;
        }
        self.published.clone_from(&self.details);
        self.last_published = now;
        Some(KeepPeekEvent::TimelineEventImages {
            event: Box::new(self.event.clone()),
            images: Vec::new(),
        })
    }
}

impl fmt::Debug for Tracker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Tracker")
            .field("active_count", &self.active.len())
            .field("baseline_count", &self.baselines.len())
            .field("pending_count", &self.pending.len())
            .field("history", &self.history)
            .field("deduplicated", &self.deduplicated)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use chrono::DateTime;
    use onvif::event::{Frame, Metadata, Notification, parse_notifications};

    use super::Tracker;
    use crate::cameras::events::EventConfig;
    use crate::keeppeek::KeepPeekEvent;

    pub(super) const BASE_MS: i64 = 1_788_566_400_000;

    pub(super) fn notification(operation: &str, state: bool, offset_ms: i64) -> Notification {
        message(
            "VideoSource/MotionAlarm",
            Some(operation),
            &format!(r#"<tt:SimpleItem Name="State" Value="{state}"/>"#),
            offset_ms,
        )
    }

    pub(super) fn message(
        topic: &str,
        operation: Option<&str>,
        data: &str,
        offset_ms: i64,
    ) -> Notification {
        parse_notifications(notification_xml(topic, operation, data, offset_ms).as_bytes())
            .unwrap()
            .remove(0)
    }

    pub(super) fn notification_xml(
        topic: &str,
        operation: Option<&str>,
        data: &str,
        offset_ms: i64,
    ) -> String {
        let timestamp = DateTime::from_timestamp_millis(BASE_MS + offset_ms)
            .unwrap()
            .to_rfc3339();
        let operation = operation
            .map(|value| format!(r#"PropertyOperation="{value}""#))
            .unwrap_or_default();
        format!(
            r#"<wsnt:NotificationMessage xmlns:wsnt="http://docs.oasis-open.org/wsn/b-2"
                xmlns:tt="http://www.onvif.org/ver10/schema"
                xmlns:tns="http://www.onvif.org/ver10/topics">
              <wsnt:Topic Dialect="http://www.onvif.org/ver10/tev/topicExpression/ConcreteSet">tns:{topic}</wsnt:Topic>
              <wsnt:Message><tt:Message UtcTime="{timestamp}" {operation}>
                <tt:Source><tt:SimpleItem Name="VideoSourceToken" Value="source-token"/>
                  <tt:SimpleItem Name="Rule" Value="perimeter"/></tt:Source>
                <tt:Key><tt:SimpleItem Name="ObjectId" Value="property-key"/></tt:Key>
                <tt:Data>{data}</tt:Data>
              </tt:Message></wsnt:Message>
            </wsnt:NotificationMessage>"#
        )
    }

    pub(super) fn frame(offset_ms: i64, module: &str, body: &str) -> Frame {
        let timestamp = DateTime::from_timestamp_millis(BASE_MS + offset_ms)
            .unwrap()
            .to_rfc3339();
        let xml = format!(
            r#"<tt:MetadataStream xmlns:tt="http://www.onvif.org/ver10/schema">
          <tt:VideoAnalytics><tt:Frame UtcTime="{timestamp}" Source="{module}">
            {body}
          </tt:Frame></tt:VideoAnalytics></tt:MetadataStream>"#
        );
        Metadata::parse(xml.as_bytes()).unwrap().frames.remove(0)
    }

    pub(super) fn object(id: &str, class: &str, confidence: &str) -> String {
        format!(
            r#"<tt:Object ObjectId="{id}"><tt:Appearance>
          <tt:Class><tt:Type Likelihood="{confidence}">{class}</tt:Type></tt:Class>
        </tt:Appearance></tt:Object>"#
        )
    }

    pub(super) fn started(changes: &[KeepPeekEvent]) -> &crate::storage::metadata::TimelineEvent {
        match changes {
            [KeepPeekEvent::TimelineEventStarted { event }] => event,
            _ => panic!("expected exactly one event opening"),
        }
    }

    #[test]
    fn partial_frames_and_deletions_keep_analytics_modules_independent() {
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        let object = object("7", "Human", "0.8");
        let first = tracker
            .frame(&frame(0, "first", &object), received, BASE_MS)
            .unwrap();
        let second = tracker
            .frame(&frame(0, "second", &object), received, BASE_MS)
            .unwrap();
        assert_ne!(started(&first).id, started(&second).id);
        assert!(
            tracker
                .frame(&frame(100, "first", ""), received, BASE_MS + 100)
                .unwrap()
                .is_empty()
        );
        assert_eq!(tracker.active_count(), 2);
        let deleted = tracker
            .frame(
                &frame(
                    200,
                    "first",
                    r#"<tt:ObjectTree><tt:Delete ObjectId="7"/></tt:ObjectTree>"#,
                ),
                received + Duration::from_millis(200),
                BASE_MS + 200,
            )
            .unwrap();
        assert!(
            matches!(deleted.as_slice(), [KeepPeekEvent::TimelineEventEnded { id, .. }]
            if id == &started(&first).id)
        );
        assert_eq!(tracker.active_count(), 1);
        assert!(
            tracker
                .expire(received + Duration::from_millis(4_999))
                .is_empty()
        );
        let expired = tracker.expire(received + Duration::from_secs(5));
        assert!(
            matches!(expired.as_slice(), [KeepPeekEvent::TimelineEventEnded { id, end_time_ms }]
            if id == &started(&second).id && *end_time_ms == BASE_MS)
        );
    }

    #[test]
    fn metadata_class_changes_close_then_open_without_per_frame_events() {
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        let first = tracker
            .frame(
                &frame(0, "module", &object("7", "Human", "0.8")),
                received,
                BASE_MS,
            )
            .unwrap();
        let same = tracker
            .frame(
                &frame(100, "module", &object("7", "Human", "0.8")),
                received + Duration::from_millis(100),
                BASE_MS + 100,
            )
            .unwrap();
        assert!(same.is_empty());
        let changed = tracker
            .frame(
                &frame(200, "module", &object("7", "Vehicle", "0.8")),
                received + Duration::from_millis(200),
                BASE_MS + 200,
            )
            .unwrap();
        assert!(matches!(changed.as_slice(), [
            KeepPeekEvent::TimelineEventEnded { id, .. },
            KeepPeekEvent::TimelineEventStarted { event },
        ] if id == &started(&first).id && event.kind == "vehicle" && event.id != *id));
        assert_eq!(tracker.active_count(), 1);
    }

    #[test]
    fn metadata_loss_does_not_close_properties_and_unknown_classes_do_not_open() {
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        tracker
            .apply(&notification("Changed", true, 0), received, BASE_MS)
            .unwrap();
        let unknown = tracker
            .frame(
                &frame(0, "module", &object("8", "Unknown", "1")),
                received,
                BASE_MS,
            )
            .unwrap();
        assert!(unknown.is_empty());
        let metadata = tracker
            .frame(
                &frame(0, "module", &object("7", "Human", "0.8")),
                received,
                BASE_MS,
            )
            .unwrap();
        let ended = tracker.disconnect_metadata("RTP packet loss");
        assert!(
            matches!(ended.as_slice(), [KeepPeekEvent::TimelineEventEnded { id, .. }]
            if id == &started(&metadata).id)
        );
        assert_eq!(tracker.kinds(), vec!["motion"]);
        assert!(
            tracker
                .expire(received + Duration::from_secs(29))
                .is_empty()
        );
        assert_eq!(tracker.expire(received + Duration::from_secs(30)).len(), 1);
    }

    #[test]
    fn initialized_activity_requires_a_false_transition_before_opening() {
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        for (offset, operation, state) in [
            (0, "Initialized", true),
            (100, "Changed", true),
            (200, "Changed", false),
        ] {
            assert!(
                tracker
                    .apply(
                        &notification(operation, state, offset),
                        received + Duration::from_millis(offset as u64),
                        BASE_MS + offset,
                    )
                    .unwrap()
                    .is_empty()
            );
        }
        let changes = tracker
            .apply(
                &notification("Changed", true, 300),
                received + Duration::from_millis(300),
                BASE_MS + 300,
            )
            .unwrap();
        assert!(matches!(
            changes.as_slice(),
            [KeepPeekEvent::TimelineEventStarted { .. }]
        ));
        assert_eq!(tracker.active_count(), 1);
        assert_eq!(tracker.kinds(), vec!["motion"]);
    }

    #[test]
    fn disconnect_ends_at_last_observation_and_rejects_replayed_timestamps() {
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        tracker
            .apply(&notification("Changed", true, 0), received, BASE_MS)
            .unwrap();
        assert!(
            tracker
                .apply(
                    &notification("Changed", true, 1_000),
                    received + Duration::from_secs(1),
                    BASE_MS + 1_000,
                )
                .unwrap()
                .is_empty()
        );
        let ended = tracker.disconnect("connection lost");
        assert!(
            matches!(ended.as_slice(), [KeepPeekEvent::TimelineEventEnded {
            end_time_ms, ..
        }] if *end_time_ms == BASE_MS + 1_000)
        );
        for offset in [0, 500, 1_000] {
            assert!(
                tracker
                    .apply(
                        &notification("Changed", true, offset),
                        received + Duration::from_secs(2),
                        BASE_MS + 2_000,
                    )
                    .unwrap()
                    .is_empty()
            );
        }
        let reopened = tracker
            .apply(
                &notification("Changed", true, 2_000),
                received + Duration::from_secs(2),
                BASE_MS + 2_000,
            )
            .unwrap();
        assert!(matches!(
            reopened.as_slice(),
            [KeepPeekEvent::TimelineEventStarted { .. }]
        ));
        assert_eq!(tracker.deduplicated(), 3);
    }
}
