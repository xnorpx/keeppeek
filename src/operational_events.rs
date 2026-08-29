use crate::{
    config::OperationalEventsConfig,
    event_forwarder::Handle as EventForwarderHandle,
    health::{CameraHealth, CameraHealthReason, CameraHealthState, StreamHealth},
    notifications::{
        Handle as NotificationHandle, Lifecycle as NotificationLifecycle,
        Stage as NotificationStage,
        model::{Candidate as NotificationCandidate, Severity, Trigger},
    },
    shutdown::Shutdown,
    storage::EventStore,
};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_AFFECTED_STREAMS: usize = 8;
const MAX_CAUSE_BYTES: usize = 64;
const MAX_EXPLANATION_BYTES: usize = 256;
// Covers several transitions for a 127-camera fleet while keeping retries to a few MiB.
const MAX_PENDING_TRANSITIONS: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationalEventKind {
    CameraOffline,
    StreamStale,
    DecodeUnavailable,
    RecordingInterrupted,
}

impl OperationalEventKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CameraOffline => "camera_offline",
            Self::StreamStale => "stream_stale",
            Self::DecodeUnavailable => "decode_unavailable",
            Self::RecordingInterrupted => "recording_interrupted",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "camera_offline" => Some(Self::CameraOffline),
            "stream_stale" => Some(Self::StreamStale),
            "decode_unavailable" => Some(Self::DecodeUnavailable),
            "recording_interrupted" => Some(Self::RecordingInterrupted),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct OperationalEventKey {
    pub camera_id: String,
    pub stream_id: Option<String>,
    pub kind: OperationalEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OperationalEvidence {
    pub cause: String,
    pub explanation: String,
    pub affected_streams: Vec<String>,
    pub recording_interrupted: bool,
    pub source: String,
}

impl OperationalEvidence {
    pub fn bounded(mut self) -> Self {
        self.cause = bounded_text(&self.cause, MAX_CAUSE_BYTES);
        self.explanation = bounded_text(&self.explanation, MAX_EXPLANATION_BYTES);
        self.source = bounded_text(&self.source, MAX_CAUSE_BYTES);
        self.affected_streams.sort_unstable();
        self.affected_streams.dedup();
        self.affected_streams.truncate(MAX_AFFECTED_STREAMS);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationalSeverity {
    Warning,
    Critical,
}

impl OperationalSeverity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "warning" => Some(Self::Warning),
            "critical" => Some(Self::Critical),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OperationalEvent {
    pub id: String,
    #[serde(flatten)]
    pub key: OperationalEventKey,
    pub evidence: OperationalEvidence,
    pub severity: OperationalSeverity,
    pub revision: u64,
    pub start_time_ms: i64,
    pub end_time_ms: Option<i64>,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationalTransitionKind {
    Started,
    Updated,
    Recovered,
    Flap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationalTransition {
    pub kind: OperationalTransitionKind,
    pub event: OperationalEvent,
    pub occurred_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationalEventPolicy {
    pub warning_hold_down: Duration,
    pub outage_hold_down: Duration,
    pub recovery_debounce: Duration,
    pub record_short_flaps: bool,
}

struct TrackedInterval {
    observed_at: Duration,
    elapsed_before_observation: Duration,
    start_time_ms: i64,
    evidence: OperationalEvidence,
    event: Option<OperationalEvent>,
    recovery_started: Option<(Duration, i64)>,
}

impl TrackedInterval {
    const fn elapsed(&self, monotonic_now: Duration) -> Duration {
        self.elapsed_before_observation
            .saturating_add(monotonic_now.saturating_sub(self.observed_at))
    }
}

pub struct OperationalEventTracker {
    policy: OperationalEventPolicy,
    intervals: HashMap<OperationalEventKey, TrackedInterval>,
}

impl OperationalEventTracker {
    pub fn new(mut policy: OperationalEventPolicy) -> Self {
        policy.outage_hold_down = policy.outage_hold_down.max(policy.warning_hold_down);
        Self {
            policy,
            intervals: HashMap::new(),
        }
    }

    pub fn set_policy(&mut self, mut policy: OperationalEventPolicy) {
        policy.outage_hold_down = policy.outage_hold_down.max(policy.warning_hold_down);
        self.policy = policy;
    }

    pub fn restore(&mut self, event: OperationalEvent, monotonic_now: Duration, unix_now_ms: i64) {
        if event.end_time_ms.is_some() {
            return;
        }
        let wall_elapsed_ms = unix_now_ms
            .saturating_sub(event.start_time_ms)
            .try_into()
            .unwrap_or(0);
        let elapsed_ms = event.duration_ms.unwrap_or(0).max(wall_elapsed_ms);
        let replace = self
            .intervals
            .get(&event.key)
            .and_then(|interval| interval.event.as_ref())
            .is_none_or(|existing| event.revision > existing.revision);
        if replace {
            self.intervals.insert(
                event.key.clone(),
                TrackedInterval {
                    observed_at: monotonic_now,
                    elapsed_before_observation: Duration::from_millis(elapsed_ms),
                    start_time_ms: event.start_time_ms,
                    evidence: event.evidence.clone(),
                    event: Some(event),
                    recovery_started: None,
                },
            );
        }
    }

    pub fn tracked_keys(&self) -> Vec<OperationalEventKey> {
        self.intervals.keys().cloned().collect()
    }

    pub fn observe_failure(
        &mut self,
        key: OperationalEventKey,
        evidence: OperationalEvidence,
        monotonic_now: Duration,
        unix_now_ms: i64,
    ) -> Option<OperationalTransition> {
        let evidence = evidence.bounded();
        let interval = self
            .intervals
            .entry(key.clone())
            .or_insert_with(|| TrackedInterval {
                observed_at: monotonic_now,
                elapsed_before_observation: Duration::ZERO,
                start_time_ms: unix_now_ms,
                evidence: evidence.clone(),
                event: None,
                recovery_started: None,
            });
        interval.recovery_started = None;
        let evidence_changed = interval.evidence != evidence;
        interval.evidence = evidence;
        let elapsed = interval.elapsed(monotonic_now);

        if interval.event.is_none() && elapsed >= self.policy.warning_hold_down {
            let event = OperationalEvent {
                id: format!("operational-{}", uuid::Uuid::new_v4()),
                key,
                evidence: interval.evidence.clone(),
                severity: if elapsed >= self.policy.outage_hold_down {
                    OperationalSeverity::Critical
                } else {
                    OperationalSeverity::Warning
                },
                revision: 1,
                start_time_ms: interval.start_time_ms,
                end_time_ms: None,
                duration_ms: Some(duration_millis(elapsed)),
            };
            interval.event = Some(event.clone());
            return Some(OperationalTransition {
                kind: OperationalTransitionKind::Started,
                occurred_at_ms: event.start_time_ms,
                event,
            });
        }

        let event = interval.event.as_mut()?;
        let escalated = event.severity == OperationalSeverity::Warning
            && elapsed >= self.policy.outage_hold_down;
        if !evidence_changed && !escalated {
            return None;
        }
        event.evidence = interval.evidence.clone();
        if escalated {
            event.severity = OperationalSeverity::Critical;
        }
        event.revision = event.revision.saturating_add(1);
        event.duration_ms = Some(duration_millis(elapsed));
        Some(OperationalTransition {
            kind: OperationalTransitionKind::Updated,
            occurred_at_ms: unix_now_ms,
            event: event.clone(),
        })
    }

    pub fn observe_recovery(
        &mut self,
        key: &OperationalEventKey,
        monotonic_now: Duration,
        unix_now_ms: i64,
    ) -> Option<OperationalTransition> {
        let interval = self.intervals.get_mut(key)?;
        let (recovery_started_at, recovery_time_ms) = *interval
            .recovery_started
            .get_or_insert((monotonic_now, unix_now_ms));
        if monotonic_now.saturating_sub(recovery_started_at) < self.policy.recovery_debounce {
            return None;
        }
        if interval.event.is_none() {
            let duration_ms = duration_millis(interval.elapsed(recovery_started_at));
            if self.policy.record_short_flaps {
                let event = OperationalEvent {
                    id: format!("operational-{}", uuid::Uuid::new_v4()),
                    key: key.clone(),
                    evidence: interval.evidence.clone(),
                    severity: OperationalSeverity::Warning,
                    revision: 2,
                    start_time_ms: interval.start_time_ms,
                    end_time_ms: Some(recovery_time_ms.max(interval.start_time_ms)),
                    duration_ms: Some(duration_ms),
                };
                self.intervals.remove(key);
                return Some(OperationalTransition {
                    kind: OperationalTransitionKind::Flap,
                    occurred_at_ms: recovery_time_ms,
                    event,
                });
            }
            tracing::debug!(
                camera.id = key.camera_id,
                camera.stream = key.stream_id.as_deref().unwrap_or("camera"),
                event.kind = key.kind.as_str(),
                event.duration_ms = duration_ms,
                "operational event candidate recovered before its hold-down"
            );
            self.intervals.remove(key);
            return None;
        }
        let mut event = interval
            .event
            .take()
            .expect("active operational interval must contain an event");
        event.revision = event.revision.saturating_add(1);
        event.end_time_ms = Some(recovery_time_ms.max(event.start_time_ms));
        event.duration_ms = Some(duration_millis(interval.elapsed(recovery_started_at)));
        self.intervals.remove(key);
        Some(OperationalTransition {
            kind: OperationalTransitionKind::Recovered,
            occurred_at_ms: recovery_time_ms,
            event,
        })
    }
}

struct PendingTransition {
    transition: OperationalTransition,
    source_name: Option<String>,
    persisted: bool,
    notifications_published: bool,
}

impl PendingTransition {
    const fn new(transition: OperationalTransition, source_name: Option<String>) -> Self {
        Self {
            transition,
            source_name,
            persisted: false,
            notifications_published: false,
        }
    }
}

struct OperationalEventEngine {
    config: OperationalEventsConfig,
    trackers: HashMap<String, OperationalEventTracker>,
}

impl OperationalEventEngine {
    fn new(config: OperationalEventsConfig) -> Self {
        Self {
            config,
            trackers: HashMap::new(),
        }
    }

    fn restore(
        &mut self,
        events: Vec<OperationalEvent>,
        monotonic_now: Duration,
        unix_now_ms: i64,
    ) -> Vec<PendingTransition> {
        let mut pending = Vec::with_capacity(events.len());
        for event in events {
            let camera_id = event.key.camera_id.clone();
            let policy = self.config.policy_for(&camera_id, &camera_id);
            self.trackers
                .entry(camera_id)
                .or_insert_with(|| OperationalEventTracker::new(policy))
                .restore(event.clone(), monotonic_now, unix_now_ms);
            pending.push(PendingTransition::new(
                OperationalTransition {
                    kind: if event.revision == 1 {
                        OperationalTransitionKind::Started
                    } else {
                        OperationalTransitionKind::Updated
                    },
                    occurred_at_ms: unix_now_ms,
                    event,
                },
                None,
            ));
        }
        pending
    }

    fn observe(
        &mut self,
        cameras: &[CameraHealth],
        monotonic_now: Duration,
        unix_now_ms: i64,
    ) -> Vec<PendingTransition> {
        let configured = cameras
            .iter()
            .map(|camera| camera.id.as_str())
            .collect::<HashSet<_>>();
        let mut pending = Vec::new();
        for camera in cameras {
            let policy = self.config.policy_for(&camera.id, &camera.ip);
            let tracker = self
                .trackers
                .entry(camera.id.clone())
                .or_insert_with(|| OperationalEventTracker::new(policy));
            tracker.set_policy(policy);
            if matches!(
                camera.state,
                CameraHealthState::Starting | CameraHealthState::Unknown
            ) {
                continue;
            }
            let failures = operational_failures(camera);
            let mut failed = failures.into_iter().collect::<Vec<_>>();
            failed.sort_unstable_by(|left, right| left.0.cmp(&right.0));
            let failed_keys = failed
                .iter()
                .map(|(key, _)| key.clone())
                .collect::<HashSet<_>>();
            for (key, evidence) in failed {
                if let Some(transition) =
                    tracker.observe_failure(key, evidence, monotonic_now, unix_now_ms)
                {
                    pending.push(PendingTransition::new(
                        transition,
                        Some(camera.name.clone()),
                    ));
                }
            }
            let mut recovered = tracker
                .tracked_keys()
                .into_iter()
                .filter(|key| !failed_keys.contains(key))
                .collect::<Vec<_>>();
            recovered.sort_unstable();
            for key in recovered {
                if let Some(transition) = tracker.observe_recovery(&key, monotonic_now, unix_now_ms)
                {
                    pending.push(PendingTransition::new(
                        transition,
                        Some(camera.name.clone()),
                    ));
                }
            }
        }

        let mut removed = self
            .trackers
            .keys()
            .filter(|camera_id| !configured.contains(camera_id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        removed.sort_unstable();
        for camera_id in removed {
            let empty = {
                let tracker = self
                    .trackers
                    .get_mut(&camera_id)
                    .expect("removed camera tracker must exist");
                for key in tracker.tracked_keys() {
                    if let Some(transition) =
                        tracker.observe_recovery(&key, monotonic_now, unix_now_ms)
                    {
                        pending.push(PendingTransition::new(transition, None));
                    }
                }
                tracker.tracked_keys().is_empty()
            };
            if empty {
                self.trackers.remove(&camera_id);
            }
        }
        pending
    }
}

pub struct OperationalEventMonitor {
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl OperationalEventMonitor {
    pub fn start<F>(
        config: OperationalEventsConfig,
        store: EventStore,
        notifications: NotificationHandle,
        event_forwarder: EventForwarderHandle,
        shutdown: Shutdown,
        snapshot: F,
    ) -> anyhow::Result<Self>
    where
        F: Fn() -> anyhow::Result<Vec<CameraHealth>> + Send + 'static,
    {
        let restored = store.open_operational_events()?;
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let thread = std::thread::Builder::new()
            .name("operational-events".to_owned())
            .spawn(move || {
                let started_at = Instant::now();
                let mut engine = OperationalEventEngine::new(config);
                let mut pending = VecDeque::new();
                extend_pending(
                    &mut pending,
                    engine.restore(restored, started_at.elapsed(), unix_time_ms()),
                );
                while !shutdown.is_cancelled() && !worker_cancel.load(Ordering::Acquire) {
                    flush_pending(&mut pending, &store, &notifications, &event_forwarder);
                    match snapshot() {
                        Ok(cameras) => extend_pending(
                            &mut pending,
                            engine.observe(&cameras, started_at.elapsed(), unix_time_ms()),
                        ),
                        Err(error) => {
                            tracing::warn!(%error, "unable to project operational camera health");
                        }
                    }
                    flush_pending(&mut pending, &store, &notifications, &event_forwarder);
                    std::thread::sleep(Duration::from_millis(500));
                }
                flush_pending(&mut pending, &store, &notifications, &event_forwarder);
            })?;
        Ok(Self {
            cancel,
            thread: Some(thread),
        })
    }

    pub fn join(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("operational event monitor panicked");
        }
    }
}

impl Drop for OperationalEventMonitor {
    fn drop(&mut self) {
        self.stop();
    }
}

fn flush_pending(
    pending: &mut VecDeque<PendingTransition>,
    store: &EventStore,
    notifications: &NotificationHandle,
    event_forwarder: &EventForwarderHandle,
) {
    flush_pending_with(
        pending,
        |event| store.upsert_operational_event(event.clone()),
        |candidate| notifications.publish(candidate),
        |transition| event_forwarder.publish_operational(transition),
    );
}

fn extend_pending(
    pending: &mut VecDeque<PendingTransition>,
    transitions: impl IntoIterator<Item = PendingTransition>,
) {
    for transition in transitions {
        if pending.len() == MAX_PENDING_TRANSITIONS {
            tracing::error!(
                event_id = %transition.transition.event.id,
                revision = transition.transition.event.revision,
                "operational event transition dropped because the retry queue is full"
            );
            continue;
        }
        pending.push_back(transition);
    }
}

fn flush_pending_with(
    pending: &mut VecDeque<PendingTransition>,
    mut persist: impl FnMut(&OperationalEvent) -> anyhow::Result<()>,
    mut publish_notification: impl FnMut(NotificationCandidate),
    mut forward: impl FnMut(&OperationalTransition) -> anyhow::Result<()>,
) {
    for item in pending.iter_mut() {
        if item.persisted {
            continue;
        }
        if let Err(error) = persist(&item.transition.event) {
            tracing::warn!(%error, "unable to persist operational event transition");
            break;
        }
        item.persisted = true;
    }

    for item in pending.iter_mut().take_while(|item| item.persisted) {
        if !item.notifications_published {
            for candidate in notification_candidates(&item.transition, item.source_name.clone()) {
                publish_notification(candidate);
            }
            item.notifications_published = true;
        }
    }

    while pending.front().is_some_and(|item| item.persisted) {
        let item = pending
            .front()
            .expect("persisted operational transition must remain queued");
        if let Err(error) = forward(&item.transition) {
            tracing::warn!(
                event_id = %item.transition.event.id,
                revision = item.transition.event.revision,
                %error,
                "unable to enqueue operational event for MQTT forwarding"
            );
            break;
        }
        pending.pop_front();
    }
}

fn notification_candidates(
    transition: &OperationalTransition,
    source_name: Option<String>,
) -> Vec<NotificationCandidate> {
    let candidate = |trigger, revision, stage, occurred_at_ms, duration_ms| NotificationCandidate {
        trigger,
        source_id: transition.event.key.camera_id.clone(),
        source_name: source_name.clone(),
        source_identity: transition.event.id.clone(),
        lifecycle: NotificationLifecycle::Outage,
        event_kind: Some(transition.event.key.kind.as_str().to_owned()),
        payload: Some(serde_json::json!({
            "cause": transition.event.evidence.cause,
            "explanation": transition.event.evidence.explanation,
            "affected_streams": transition.event.evidence.affected_streams,
            "recording_interrupted": transition.event.evidence.recording_interrupted,
            "evidence_source": transition.event.evidence.source,
            "stream_id": transition.event.key.stream_id,
        })),
        group_ids: Vec::new(),
        zone: None,
        confidence: None,
        attachment_path: None,
        canonical_attachment: None,
        icon_key: Some("alert".to_owned()),
        image_available: false,
        duration_ms,
        severity: match transition.event.severity {
            OperationalSeverity::Warning => Severity::Warning,
            OperationalSeverity::Critical => Severity::Critical,
        },
        reviewed: None,
        bookmarked: None,
        privacy_active: false,
        revision,
        stage,
        occurred_at_ms,
        deep_link: camera_deep_link(&transition.event.key.camera_id),
    };
    match transition.kind {
        OperationalTransitionKind::Started => vec![candidate(
            Trigger::OutageStarted,
            transition.event.revision,
            NotificationStage::Preliminary,
            transition.event.start_time_ms,
            transition.event.duration_ms,
        )],
        OperationalTransitionKind::Updated => vec![candidate(
            Trigger::EventUpdated,
            transition.event.revision,
            NotificationStage::Enriched,
            transition.occurred_at_ms,
            transition.event.duration_ms,
        )],
        OperationalTransitionKind::Recovered => vec![candidate(
            Trigger::Recovery,
            transition.event.revision,
            NotificationStage::Recovery,
            transition.occurred_at_ms,
            transition.event.duration_ms,
        )],
        OperationalTransitionKind::Flap => vec![
            candidate(
                Trigger::OutageStarted,
                1,
                NotificationStage::Preliminary,
                transition.event.start_time_ms,
                transition.event.duration_ms,
            ),
            candidate(
                Trigger::Recovery,
                transition.event.revision,
                NotificationStage::Recovery,
                transition.occurred_at_ms,
                transition.event.duration_ms,
            ),
        ],
    }
}

fn camera_deep_link(camera_id: &str) -> String {
    let encoded = url::form_urlencoded::byte_serialize(camera_id.as_bytes()).collect::<String>();
    format!("/camera?camera={encoded}")
}

fn unix_time_ms() -> i64 {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(milliseconds).unwrap_or(i64::MAX)
}

pub fn operational_failures(
    camera: &CameraHealth,
) -> HashMap<OperationalEventKey, OperationalEvidence> {
    if !camera.dimensions.expected
        || camera.dimensions.battery_sleeping == Some(true)
        || matches!(
            camera.state,
            CameraHealthState::Starting | CameraHealthState::Stopped | CameraHealthState::Unknown
        )
    {
        return HashMap::new();
    }

    let mut failures = HashMap::new();
    let all_streams = camera.dimensions.configured_video_stream_ids.clone();
    let transport_disconnected = camera.dimensions.connected_video_streams == Some(0)
        || (camera.dimensions.connected_video_streams.is_none()
            && camera.dimensions.transport_connected == Some(false));
    if transport_disconnected {
        failures.insert(
            OperationalEventKey {
                camera_id: camera.id.clone(),
                stream_id: None,
                kind: OperationalEventKind::CameraOffline,
            },
            OperationalEvidence {
                cause: camera.reason.as_str().to_owned(),
                explanation: camera.detail.clone(),
                affected_streams: all_streams.clone(),
                recording_interrupted: camera.dimensions.recording_requested,
                source: "canonical_health".to_owned(),
            }
            .bounded(),
        );
    }

    for stream_id in &all_streams {
        let stream = camera
            .streams
            .iter()
            .find(|stream| health_stream_id(stream) == Some(stream_id.as_str()));
        if stream
            .is_none_or(|stream| !stream.dimensions.report_fresh || !stream.dimensions.frames_fresh)
        {
            let cause = stream.map_or("stream_report_missing", |stream| stream.reason.as_str());
            let explanation = stream.map_or("No current stream evidence", |stream| {
                stream.detail.as_str()
            });
            failures.insert(
                OperationalEventKey {
                    camera_id: camera.id.clone(),
                    stream_id: Some(stream_id.clone()),
                    kind: OperationalEventKind::StreamStale,
                },
                OperationalEvidence {
                    cause: cause.to_owned(),
                    explanation: explanation.to_owned(),
                    affected_streams: vec![stream_id.clone()],
                    recording_interrupted: camera
                        .dimensions
                        .recording_video_stream_ids
                        .contains(stream_id),
                    source: "canonical_health".to_owned(),
                }
                .bounded(),
            );
        }
        if stream.is_some_and(|stream| !stream.dimensions.decodable) {
            failures.insert(
                OperationalEventKey {
                    camera_id: camera.id.clone(),
                    stream_id: Some(stream_id.clone()),
                    kind: OperationalEventKind::DecodeUnavailable,
                },
                OperationalEvidence {
                    cause: CameraHealthReason::KeyframesMissing.as_str().to_owned(),
                    explanation: CameraHealthReason::KeyframesMissing.detail().to_owned(),
                    affected_streams: vec![stream_id.clone()],
                    recording_interrupted: camera
                        .dimensions
                        .recording_video_stream_ids
                        .contains(stream_id),
                    source: "canonical_health".to_owned(),
                }
                .bounded(),
            );
        }
    }

    for stream_id in &camera.dimensions.recording_video_stream_ids {
        if camera
            .dimensions
            .recording_progressing_stream_ids
            .contains(stream_id)
        {
            continue;
        }
        let stream = camera
            .streams
            .iter()
            .find(|stream| health_stream_id(stream) == Some(stream_id.as_str()));
        if stream.and_then(|stream| stream.dimensions.recording_progressing) != Some(false) {
            continue;
        }
        failures.insert(
            OperationalEventKey {
                camera_id: camera.id.clone(),
                stream_id: Some(stream_id.clone()),
                kind: OperationalEventKind::RecordingInterrupted,
            },
            OperationalEvidence {
                cause: "recording_not_progressing".to_owned(),
                explanation: stream.map_or_else(
                    || "Requested recording writes are not progressing".to_owned(),
                    |stream| stream.detail.clone(),
                ),
                affected_streams: vec![stream_id.clone()],
                recording_interrupted: true,
                source: "recording_writer".to_owned(),
            }
            .bounded(),
        );
    }
    failures
}

fn health_stream_id(stream: &StreamHealth) -> Option<&str> {
    stream.ingress.report.kind.strip_prefix("video_")
}

fn bounded_text(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        health::{CameraHealthDimensions, StreamHealthDimensions},
        stats::{StreamHealthReport, StreamReport},
    };

    fn key(stream_id: Option<&str>) -> OperationalEventKey {
        OperationalEventKey {
            camera_id: "front-door".to_owned(),
            stream_id: stream_id.map(str::to_owned),
            kind: OperationalEventKind::StreamStale,
        }
    }

    fn evidence(cause: &str) -> OperationalEvidence {
        OperationalEvidence {
            cause: cause.to_owned(),
            explanation: format!("stream is unavailable because {cause}"),
            affected_streams: vec!["sub".to_owned()],
            recording_interrupted: true,
            source: "canonical_health".to_owned(),
        }
    }

    fn pending_transition(event_id: &str, revision: u64) -> PendingTransition {
        PendingTransition::new(
            OperationalTransition {
                kind: OperationalTransitionKind::Updated,
                event: OperationalEvent {
                    id: event_id.to_owned(),
                    key: key(Some("sub")),
                    evidence: evidence("frames_stale"),
                    severity: OperationalSeverity::Warning,
                    revision,
                    start_time_ms: 1_000,
                    end_time_ms: None,
                    duration_ms: Some(1_000),
                },
                occurred_at_ms: 2_000,
            },
            Some("Front Door".to_owned()),
        )
    }

    #[test]
    fn mqtt_failure_does_not_block_persistence_or_notifications() {
        let mut pending = VecDeque::from([
            pending_transition("event-a", 1),
            pending_transition("event-b", 1),
        ]);
        let mut persisted = Vec::new();
        let mut notified = Vec::new();
        let mut forwarded = Vec::new();

        flush_pending_with(
            &mut pending,
            |event| {
                persisted.push(event.id.clone());
                Ok(())
            },
            |candidate| notified.push(candidate.source_identity),
            |transition| {
                forwarded.push(transition.event.id.clone());
                anyhow::bail!("MQTT ingest unavailable")
            },
        );

        assert_eq!(persisted, ["event-a", "event-b"]);
        assert_eq!(notified, ["event-a", "event-b"]);
        assert_eq!(forwarded, ["event-a"]);
        assert_eq!(pending.len(), 2);

        flush_pending_with(
            &mut pending,
            |event| {
                persisted.push(event.id.clone());
                Ok(())
            },
            |candidate| notified.push(candidate.source_identity),
            |transition| {
                forwarded.push(transition.event.id.clone());
                Ok(())
            },
        );

        assert_eq!(persisted, ["event-a", "event-b"]);
        assert_eq!(notified, ["event-a", "event-b"]);
        assert_eq!(forwarded, ["event-a", "event-a", "event-b"]);
        assert!(pending.is_empty());
    }

    #[test]
    fn persistence_failure_preserves_transition_order() {
        let mut pending = VecDeque::from([
            pending_transition("event-a", 1),
            pending_transition("event-b", 2),
        ]);
        let mut persisted = Vec::new();
        let mut notified = Vec::new();
        let mut forwarded = Vec::new();

        flush_pending_with(
            &mut pending,
            |event| {
                persisted.push(event.id.clone());
                anyhow::bail!("catalog unavailable")
            },
            |candidate| notified.push(candidate.source_identity),
            |transition| {
                forwarded.push(transition.event.id.clone());
                Ok(())
            },
        );

        assert_eq!(persisted, ["event-a"]);
        assert!(notified.is_empty());
        assert!(forwarded.is_empty());
        assert_eq!(pending.len(), 2);
    }

    #[test]
    fn pending_transition_queue_is_bounded() {
        let mut pending = VecDeque::new();
        extend_pending(
            &mut pending,
            (0..=MAX_PENDING_TRANSITIONS)
                .map(|index| pending_transition(&format!("event-{index}"), 1)),
        );

        assert_eq!(pending.len(), MAX_PENDING_TRANSITIONS);
        assert_eq!(pending.front().unwrap().transition.event.id, "event-0");
        assert_eq!(
            pending.back().unwrap().transition.event.id,
            format!("event-{}", MAX_PENDING_TRANSITIONS - 1)
        );
    }

    fn tracker() -> OperationalEventTracker {
        OperationalEventTracker::new(OperationalEventPolicy {
            warning_hold_down: Duration::from_secs(10),
            outage_hold_down: Duration::from_secs(30),
            recovery_debounce: Duration::from_secs(5),
            record_short_flaps: false,
        })
    }

    fn stream(
        stream_id: &str,
        frames_fresh: bool,
        decodable: bool,
        recording_requested: bool,
        recording_progressing: Option<bool>,
    ) -> StreamHealth {
        let reason = if !frames_fresh {
            CameraHealthReason::FramesNotArriving
        } else if !decodable {
            CameraHealthReason::KeyframesMissing
        } else if recording_progressing == Some(false) {
            CameraHealthReason::RecordingNotProgressing
        } else {
            CameraHealthReason::Healthy
        };
        StreamHealth {
            ingress: StreamHealthReport {
                report: StreamReport {
                    kind: format!("video_{stream_id}"),
                    session_duration_ms: 10_000,
                    codec: Some("h264".to_owned()),
                    resolution: Some("1920x1080".to_owned()),
                    fps: 15.0,
                    expected_fps: 15.0,
                    kf_fps: 1.0,
                    kbps: 1_000.0,
                    max_frame_kb: 100.0,
                    gap_min_ms: 60.0,
                    gap_avg_ms: 66.0,
                    gap_max_ms: 80.0,
                    jitter_samples: 100,
                    jitter_p50_ms: 1.0,
                    jitter_p99_ms: 2.0,
                    frames: Some(150),
                    bytes: Some(1_000_000),
                    keyframes: Some(10),
                    reconnects: None,
                    drops: None,
                    errors: None,
                },
                updated_at_ms: 10_000,
                report_age_ms: 100,
                frame_updated_at_ms: Some(10_000),
                frame_age_ms: Some(100),
                keyframe_updated_at_ms: Some(10_000),
                keyframe_age_ms: Some(100),
                recent_reconnects: 0,
                recent_drops: 0,
                recent_errors: 0,
            },
            state: if reason == CameraHealthReason::Healthy {
                CameraHealthState::Healthy
            } else if frames_fresh {
                CameraHealthState::Degraded
            } else {
                CameraHealthState::Stale
            },
            reason,
            reason_codes: vec![reason],
            detail: reason.detail().to_owned(),
            dimensions: StreamHealthDimensions {
                expected: true,
                transport_connected: Some(true),
                report_fresh: true,
                report_freshness_threshold_ms: 30_000,
                frames_fresh,
                frame_freshness_threshold_ms: 30_000,
                decodable,
                keyframe_freshness_threshold_ms: 30_000,
                recent_reconnects: 0,
                recent_drops: 0,
                recent_errors: 0,
                recording_requested,
                recording_progressing,
                recording_progress_age_ms: Some(100),
                session_duration_ms: 10_000,
                recorded_duration_ms: 10_000,
            },
        }
    }

    fn camera(
        streams: Vec<StreamHealth>,
        connected_video_streams: Option<usize>,
        transport_connected: Option<bool>,
    ) -> CameraHealth {
        let stream_ids = streams
            .iter()
            .filter_map(health_stream_id)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let fresh_stream_ids = streams
            .iter()
            .filter(|stream| stream.dimensions.frames_fresh)
            .filter_map(health_stream_id)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let decodable_stream_ids = streams
            .iter()
            .filter(|stream| stream.dimensions.decodable)
            .filter_map(health_stream_id)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let recording_stream_ids = streams
            .iter()
            .filter(|stream| stream.dimensions.recording_requested)
            .filter_map(health_stream_id)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let recording_progressing_stream_ids = streams
            .iter()
            .filter(|stream| stream.dimensions.recording_progressing == Some(true))
            .filter_map(health_stream_id)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let recording_progressing = (!recording_stream_ids.is_empty())
            .then_some(recording_progressing_stream_ids.len() == recording_stream_ids.len());
        CameraHealth {
            id: "front-door".to_owned(),
            ip: "192.0.2.10".to_owned(),
            name: "Front Door".to_owned(),
            manufacturer: None,
            model: None,
            firmware_version: None,
            backend: "retina".to_owned(),
            transport: "tcp".to_owned(),
            state: CameraHealthState::Degraded,
            reason: CameraHealthReason::FramesNotArriving,
            reason_codes: vec![CameraHealthReason::FramesNotArriving],
            detail: CameraHealthReason::FramesNotArriving.detail().to_owned(),
            dimensions: CameraHealthDimensions {
                configured: true,
                expected: true,
                configured_video_streams: stream_ids.len(),
                connected_video_streams,
                reporting_video_streams: stream_ids.len(),
                fresh_video_streams: fresh_stream_ids.len(),
                decodable_video_streams: decodable_stream_ids.len(),
                configured_video_stream_ids: stream_ids.clone(),
                connected_video_stream_ids: connected_video_streams.map(|_| stream_ids.clone()),
                reporting_video_stream_ids: stream_ids,
                fresh_video_stream_ids: fresh_stream_ids,
                decodable_video_stream_ids: decodable_stream_ids,
                transport_connected,
                latest_report_at_ms: Some(10_000),
                report_age_ms: Some(100),
                frames_fresh: Some(true),
                decodable: Some(true),
                recent_reconnects: 0,
                recent_drops: 0,
                recent_errors: 0,
                recording_requested: !recording_stream_ids.is_empty(),
                recording_video_streams: recording_stream_ids.len(),
                recording_streams_progressing: recording_progressing_stream_ids.len(),
                recording_video_stream_ids: recording_stream_ids,
                recording_progressing_stream_ids,
                recording_progressing,
                recording_progress_age_ms: Some(100),
                session_duration_ms: Some(10_000),
                recorded_main_duration_ms: 10_000,
                recorded_sub_duration_ms: 10_000,
                recorded_total_duration_ms: 20_000,
                battery_configured: false,
                battery_registered: None,
                battery_last_seen_age_ms: None,
                battery_wake_pending_age_ms: None,
                battery_sleeping: None,
            },
            lifecycle: Some("degraded".to_owned()),
            last_error: None,
            configured_profiles: Vec::new(),
            streams,
        }
    }

    #[test]
    fn suppresses_short_flaps_and_retains_the_true_failure_time() {
        let mut tracker = tracker();
        let stream = key(Some("sub"));

        assert!(
            tracker
                .observe_failure(
                    stream.clone(),
                    evidence("frames_stale"),
                    Duration::ZERO,
                    1_000
                )
                .is_none()
        );
        assert!(
            tracker
                .observe_recovery(&stream, Duration::from_secs(9), 10_000)
                .is_none()
        );
        assert!(
            tracker
                .observe_recovery(&stream, Duration::from_secs(14), 15_000)
                .is_none()
        );
        assert!(
            tracker
                .observe_failure(
                    stream.clone(),
                    evidence("frames_stale"),
                    Duration::from_secs(20),
                    21_000,
                )
                .is_none()
        );

        let started = tracker
            .observe_failure(
                stream,
                evidence("frames_stale"),
                Duration::from_secs(30),
                31_000,
            )
            .expect("sustained failure should start an event");
        assert_eq!(started.kind, OperationalTransitionKind::Started);
        assert_eq!(started.event.start_time_ms, 21_000);
        assert_eq!(started.event.severity, OperationalSeverity::Warning);
        assert_eq!(started.event.duration_ms, Some(10_000));
    }

    #[test]
    fn updates_and_recovers_one_stable_interval() {
        let mut tracker = tracker();
        let stream = key(Some("main"));
        assert!(
            tracker
                .observe_failure(
                    stream.clone(),
                    evidence("frames_stale"),
                    Duration::ZERO,
                    1_000
                )
                .is_none()
        );
        let started = tracker
            .observe_failure(
                stream.clone(),
                evidence("frames_stale"),
                Duration::from_secs(10),
                11_000,
            )
            .expect("warning hold-down should expire");
        let event_id = started.event.id;

        let updated = tracker
            .observe_failure(
                stream.clone(),
                evidence("keyframes_missing"),
                Duration::from_secs(30),
                31_000,
            )
            .expect("changed cause and outage threshold should update the event");
        assert_eq!(updated.kind, OperationalTransitionKind::Updated);
        assert_eq!(updated.event.id, event_id);
        assert_eq!(updated.event.revision, 2);
        assert_eq!(updated.event.severity, OperationalSeverity::Critical);
        assert_eq!(updated.event.duration_ms, Some(30_000));

        assert!(
            tracker
                .observe_recovery(&stream, Duration::from_secs(40), 41_000)
                .is_none()
        );
        assert!(
            tracker
                .observe_failure(
                    stream.clone(),
                    evidence("keyframes_missing"),
                    Duration::from_secs(42),
                    43_000,
                )
                .is_none()
        );
        assert!(
            tracker
                .observe_recovery(&stream, Duration::from_secs(50), 51_000)
                .is_none()
        );
        let recovered = tracker
            .observe_recovery(&stream, Duration::from_secs(55), 56_000)
            .expect("stable recovery should close the event");
        assert_eq!(recovered.kind, OperationalTransitionKind::Recovered);
        assert_eq!(recovered.event.id, event_id);
        assert_eq!(recovered.event.revision, 3);
        assert_eq!(recovered.event.end_time_ms, Some(51_000));
        assert_eq!(recovered.event.duration_ms, Some(50_000));
    }

    #[test]
    fn restored_interval_keeps_identity_and_handles_wall_clock_rollback() {
        let mut tracker = tracker();
        let stream = key(Some("sub"));
        tracker.restore(
            OperationalEvent {
                id: "operational-existing".to_owned(),
                key: stream.clone(),
                evidence: evidence("frames_stale"),
                severity: OperationalSeverity::Critical,
                revision: 4,
                start_time_ms: 50_000,
                end_time_ms: None,
                duration_ms: Some(30_000),
            },
            Duration::ZERO,
            40_000,
        );

        assert!(
            tracker
                .observe_recovery(&stream, Duration::from_secs(1), 40_000)
                .is_none()
        );
        let recovered = tracker
            .observe_recovery(&stream, Duration::from_secs(6), 45_000)
            .expect("restored event should recover after the debounce");
        assert_eq!(recovered.event.id, "operational-existing");
        assert_eq!(recovered.event.revision, 5);
        assert_eq!(recovered.event.end_time_ms, Some(50_000));
        assert_eq!(recovered.event.duration_ms, Some(31_000));
    }

    #[test]
    fn explicitly_records_short_flaps_as_closed_intervals() {
        let mut tracker = OperationalEventTracker::new(OperationalEventPolicy {
            warning_hold_down: Duration::from_secs(10),
            outage_hold_down: Duration::from_secs(30),
            recovery_debounce: Duration::from_secs(5),
            record_short_flaps: true,
        });
        let stream = key(Some("sub"));
        tracker.observe_failure(
            stream.clone(),
            evidence("frames_stale"),
            Duration::ZERO,
            1_000,
        );
        assert!(
            tracker
                .observe_recovery(&stream, Duration::from_secs(3), 4_000)
                .is_none()
        );
        let flap = tracker
            .observe_recovery(&stream, Duration::from_secs(8), 9_000)
            .expect("configured short flap should be retained after stable recovery");
        assert_eq!(flap.kind, OperationalTransitionKind::Flap);
        assert_eq!(flap.event.revision, 2);
        assert_eq!(flap.event.duration_ms, Some(3_000));
        assert!(tracker.tracked_keys().is_empty());
    }

    #[test]
    fn projects_stale_decode_and_writer_failures_independently_per_stream() {
        let health = camera(
            vec![
                stream("main", false, false, true, Some(false)),
                stream("sub", true, true, true, None),
            ],
            Some(2),
            Some(true),
        );

        let failures = operational_failures(&health);
        assert!(failures.contains_key(&OperationalEventKey {
            camera_id: "front-door".to_owned(),
            stream_id: Some("main".to_owned()),
            kind: OperationalEventKind::StreamStale,
        }));
        assert!(failures.contains_key(&OperationalEventKey {
            camera_id: "front-door".to_owned(),
            stream_id: Some("main".to_owned()),
            kind: OperationalEventKind::DecodeUnavailable,
        }));
        assert!(failures.contains_key(&OperationalEventKey {
            camera_id: "front-door".to_owned(),
            stream_id: Some("main".to_owned()),
            kind: OperationalEventKind::RecordingInterrupted,
        }));
        assert!(
            failures
                .keys()
                .all(|key| key.stream_id.as_deref() != Some("sub"))
        );
    }

    #[test]
    fn indeterminate_startup_preserves_a_restored_interval_until_real_recovery() {
        let config = OperationalEventsConfig {
            recovery_debounce_secs: 0,
            ..OperationalEventsConfig::default()
        };
        let mut engine = OperationalEventEngine::new(config);
        let restored = OperationalEvent {
            id: "operational-existing".to_owned(),
            key: OperationalEventKey {
                camera_id: "front-door".to_owned(),
                stream_id: None,
                kind: OperationalEventKind::CameraOffline,
            },
            evidence: OperationalEvidence {
                cause: "transport_disconnected".to_owned(),
                explanation: "Camera transport is disconnected".to_owned(),
                affected_streams: vec!["main".to_owned()],
                recording_interrupted: false,
                source: "canonical_health".to_owned(),
            },
            severity: OperationalSeverity::Critical,
            revision: 4,
            start_time_ms: 1_000,
            end_time_ms: None,
            duration_ms: Some(60_000),
        };
        assert_eq!(
            engine.restore(vec![restored], Duration::ZERO, 61_000).len(),
            1
        );

        let mut starting = camera(Vec::new(), Some(0), Some(false));
        starting.state = CameraHealthState::Starting;
        starting.reason = CameraHealthReason::Starting;
        assert!(
            engine
                .observe(&[starting], Duration::from_secs(1), 62_000)
                .is_empty()
        );

        let mut healthy = camera(
            vec![stream("main", true, true, false, None)],
            Some(1),
            Some(true),
        );
        healthy.state = CameraHealthState::Healthy;
        healthy.reason = CameraHealthReason::Healthy;
        let recovered = engine.observe(&[healthy], Duration::from_secs(2), 63_000);
        assert_eq!(recovered.len(), 1);
        assert_eq!(
            recovered[0].transition.kind,
            OperationalTransitionKind::Recovered
        );
        assert_eq!(recovered[0].transition.event.id, "operational-existing");
        assert_eq!(recovered[0].transition.event.revision, 5);
    }

    #[test]
    fn projects_aggregate_transport_loss_without_collapsing_partial_connectivity() {
        let stream = stream("main", true, true, false, None);
        let disconnected = camera(vec![stream.clone()], None, Some(false));
        assert!(
            operational_failures(&disconnected).contains_key(&OperationalEventKey {
                camera_id: "front-door".to_owned(),
                stream_id: None,
                kind: OperationalEventKind::CameraOffline,
            })
        );

        let partially_connected = camera(vec![stream], Some(1), Some(false));
        assert!(
            !operational_failures(&partially_connected)
                .keys()
                .any(|key| key.kind == OperationalEventKind::CameraOffline)
        );
    }

    #[test]
    fn removed_camera_tracker_is_released_after_recovery() {
        let config = OperationalEventsConfig {
            warning_hold_down_secs: 0,
            recovery_debounce_secs: 0,
            ..OperationalEventsConfig::default()
        };
        let mut engine = OperationalEventEngine::new(config);
        let disconnected = camera(Vec::new(), Some(0), Some(false));
        assert_eq!(
            engine.observe(&[disconnected], Duration::ZERO, 1_000).len(),
            1
        );
        assert_eq!(engine.observe(&[], Duration::from_secs(1), 2_000).len(), 1);
        assert!(engine.trackers.is_empty());
    }

    #[test]
    #[ignore = "run with cargo test --release --lib operational_projection_latency -- --ignored --nocapture"]
    fn operational_projection_latency() {
        use std::hint::black_box;

        const ITERATIONS: usize = 20_000;
        const CAMERA_COUNT: usize = 64;
        const P95_BUDGET_NS: u64 = 2_500_000;

        let cameras = (0..CAMERA_COUNT)
            .map(|index| {
                let mut camera = camera(
                    vec![
                        stream("main", true, true, false, None),
                        stream("sub", true, true, false, None),
                    ],
                    Some(2),
                    Some(true),
                );
                camera.id = format!("camera-{index}");
                camera.name = format!("Camera {index}");
                camera.ip = format!("192.0.2.{}", index + 1);
                camera.state = CameraHealthState::Healthy;
                camera.reason = CameraHealthReason::Healthy;
                camera
            })
            .collect::<Vec<_>>();
        let mut engine = OperationalEventEngine::new(OperationalEventsConfig::default());
        let mut baseline = hdrhistogram::Histogram::<u64>::new(3).unwrap();
        let mut projection = hdrhistogram::Histogram::<u64>::new(3).unwrap();

        for iteration in 0..ITERATIONS {
            let started = Instant::now();
            let healthy_dimensions = black_box(&cameras)
                .iter()
                .filter(|camera| {
                    camera.dimensions.transport_connected == Some(true)
                        && camera.dimensions.frames_fresh == Some(true)
                        && camera.dimensions.decodable == Some(true)
                })
                .count();
            black_box(healthy_dimensions);
            baseline.record(elapsed_nanos(started)).unwrap();

            let elapsed = Duration::from_millis(u64::try_from(iteration).unwrap());
            let started = Instant::now();
            let transitions = engine.observe(black_box(&cameras), elapsed, 1_000);
            black_box(transitions);
            projection.record(elapsed_nanos(started)).unwrap();
        }

        let baseline_p95_ns = baseline.value_at_quantile(0.95);
        let projection_p95_ns = projection.value_at_quantile(0.95);
        println!(
            "operational_projection_latency iterations={ITERATIONS} cameras={CAMERA_COUNT} streams_per_camera=2 baseline_p50_ns={} baseline_p95_ns={baseline_p95_ns} projection_p50_ns={} projection_p95_ns={projection_p95_ns} delta_p95_ns={} budget_p95_ns={P95_BUDGET_NS}",
            baseline.value_at_quantile(0.5),
            projection.value_at_quantile(0.5),
            projection_p95_ns.saturating_sub(baseline_p95_ns),
        );
        assert!(
            projection_p95_ns <= P95_BUDGET_NS,
            "operational projection P95 {projection_p95_ns} ns exceeds {P95_BUDGET_NS} ns budget"
        );
    }

    fn elapsed_nanos(started: Instant) -> u64 {
        started.elapsed().as_nanos().try_into().unwrap_or(u64::MAX)
    }
}
