use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::time::Instant;

use chrono::{DateTime, Utc};
use onvif::event::Kind;

use super::HISTORY_MAX;
use super::observation::{Action, Identity, Observation};

pub(super) struct History {
    marks: HashMap<Identity, Mark>,
    order: VecDeque<Identity>,
    seen: HashSet<[u8; 32]>,
    recent: VecDeque<[u8; 32]>,
}

struct Mark {
    camera: DateTime<Utc>,
    received: Instant,
    in_window_time: Option<DateTime<Utc>>,
    untrusted_time: Option<DateTime<Utc>>,
    state: bool,
    opened: bool,
    kind: Option<Kind>,
}

impl History {
    pub(super) fn new() -> Self {
        Self {
            marks: HashMap::with_capacity(HISTORY_MAX),
            order: VecDeque::with_capacity(HISTORY_MAX),
            seen: HashSet::with_capacity(HISTORY_MAX),
            recent: VecDeque::with_capacity(HISTORY_MAX),
        }
    }

    pub(super) fn is_replay(&self, observation: &Observation) -> bool {
        if self.seen.contains(&observation.fingerprint()) {
            return true;
        }
        let Some(previous) = self.marks.get(&observation.key) else {
            return false;
        };
        if observation.clock.received < previous.received {
            return true;
        }
        if !observation.clock.has_camera_time() {
            return false;
        }
        let watermark = if observation.clock.camera_time_in_window() {
            previous.in_window_time
        } else {
            previous.in_window_time.max(previous.untrusted_time)
        };
        watermark.is_some_and(|watermark| {
            observation.clock.camera < watermark
                || (observation.clock.camera == watermark
                    && !(observation.action == Action::End
                        && previous.state
                        && previous.camera == observation.clock.camera))
        })
    }

    pub(super) fn open_kind(&self, key: &Identity) -> Option<Kind> {
        self.marks
            .get(key)
            .filter(|mark| mark.opened)
            .and_then(|mark| mark.kind)
    }

    pub(super) fn remember(
        &mut self,
        observation: &Observation,
        opened: bool,
        protected: impl Fn(&Identity) -> bool,
    ) {
        let fingerprint = observation.fingerprint();
        if self.recent.len() == HISTORY_MAX {
            let oldest = self
                .recent
                .pop_front()
                .expect("full replay queue has an oldest item");
            self.seen.remove(&oldest);
        }
        if self.seen.insert(fingerprint) {
            self.recent.push_back(fingerprint);
        }
        if !self.marks.contains_key(&observation.key) {
            self.evict(&protected);
            self.order.push_back(observation.key.clone());
        }
        let previous = self.marks.get(&observation.key);
        let in_window_time = if observation.clock.camera_time_in_window() {
            Some(observation.clock.camera)
        } else {
            previous.and_then(|mark| mark.in_window_time)
        };
        let untrusted_time =
            if observation.clock.camera_time_in_window() || !observation.clock.has_camera_time() {
                previous.and_then(|mark| mark.untrusted_time)
            } else {
                Some(observation.clock.camera)
            };
        self.marks.insert(
            observation.key.clone(),
            Mark {
                camera: observation.clock.camera,
                received: observation.clock.received,
                in_window_time,
                untrusted_time,
                state: observation.state(),
                opened,
                kind: observation.kind,
            },
        );
        assert_eq!(
            self.marks.len(),
            self.order.len(),
            "camera event watermark order diverged"
        );
        assert!(
            self.marks.len() <= HISTORY_MAX && self.seen.len() <= HISTORY_MAX,
            "camera event replay budget exceeded"
        );
    }

    fn evict(&mut self, protected: &impl Fn(&Identity) -> bool) {
        if self.marks.len() < HISTORY_MAX {
            return;
        }
        for _ in 0..HISTORY_MAX {
            let oldest = self
                .order
                .pop_front()
                .expect("full watermark queue has an oldest item");
            if protected(&oldest) {
                self.order.push_back(oldest);
            } else {
                self.marks.remove(&oldest);
                return;
            }
        }
        panic!("bounded active and baseline state cannot pin every watermark");
    }
}

impl fmt::Debug for History {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("History")
            .field("watermark_count", &self.marks.len())
            .field("dedupe_count", &self.seen.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::super::tests::{BASE_MS, message, notification, started};
    use super::super::{HISTORY_MAX, Tracker};
    use crate::cameras::events::EventConfig;
    use crate::keeppeek::KeepPeekEvent;

    fn fill_point_history(tracker: &mut Tracker, received: Instant) {
        for index in 0..=HISTORY_MAX {
            let offset = 2_001 + i64::try_from(index).unwrap();
            let mut point = message("RuleEngine/LineDetector/Crossed", None, "", offset);
            point.key.simple[0].value = index.to_string();
            tracker
                .apply(
                    &point,
                    received + Duration::from_millis(offset as u64),
                    BASE_MS + offset,
                )
                .unwrap();
        }
    }

    #[test]
    fn stale_timestamp_remains_fenced_after_receipt_skew_and_dedupe_eviction() {
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        tracker
            .apply(&notification("Changed", true, 0), received, BASE_MS)
            .unwrap();
        tracker
            .apply(
                &notification("Changed", true, 2_000),
                received + Duration::from_secs(2),
                BASE_MS + 2_000,
            )
            .unwrap();
        fill_point_history(&mut tracker, received);
        assert_eq!(tracker.history.marks.len(), 1024);
        assert_eq!(tracker.history.seen.len(), 1024);
        assert_eq!(tracker.disconnect("reconnect").len(), 1);
        let replay = tracker
            .apply(
                &notification("Changed", true, 0),
                received + Duration::from_secs(601),
                BASE_MS + 601_000,
            )
            .unwrap();
        assert!(replay.is_empty());
        assert_eq!(tracker.active_count(), 0);
        assert_eq!(tracker.deduplicated(), 1);
    }

    #[test]
    fn invalid_future_time_does_not_poison_a_later_valid_camera_clock() {
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        tracker
            .apply(&notification("Changed", true, 0), received, BASE_MS)
            .unwrap();
        tracker
            .apply(
                &notification("Changed", true, 1_000_000),
                received + Duration::from_secs(1),
                BASE_MS + 1_000,
            )
            .unwrap();
        tracker.disconnect("clock correction");
        let changes = tracker
            .apply(
                &notification("Changed", true, 2_000),
                received + Duration::from_secs(2),
                BASE_MS + 2_000,
            )
            .unwrap();
        assert_eq!(started(&changes).start_time_ms, BASE_MS + 2_000);
        assert_eq!(
            started(&changes).payload.as_ref().unwrap()["timestamp_source"],
            "camera"
        );
    }

    #[test]
    fn same_timestamp_stop_closes_once_and_replayed_true_cannot_reopen() {
        let received = Instant::now();
        let mut tracker = Tracker::new("camera".to_owned(), EventConfig::default(), true);
        tracker
            .apply(&notification("Changed", true, 0), received, BASE_MS)
            .unwrap();
        let stopped = tracker
            .apply(&notification("Changed", false, 0), received, BASE_MS)
            .unwrap();
        assert!(
            matches!(stopped.as_slice(), [KeepPeekEvent::TimelineEventEnded { end_time_ms, .. }] if *end_time_ms == BASE_MS)
        );
        assert!(
            tracker
                .apply(&notification("Changed", true, 0), received, BASE_MS)
                .unwrap()
                .is_empty()
        );
        assert!(
            tracker
                .apply(&notification("Changed", false, 0), received, BASE_MS)
                .unwrap()
                .is_empty()
        );
        assert_eq!(tracker.active_count(), 0);
        assert_eq!(tracker.deduplicated(), 2);
    }
}
