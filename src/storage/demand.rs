use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Debug, Serialize)]
pub(crate) struct RecordingDemandHealth {
    pub active_streams: usize,
    pub total_viewers: usize,
    pub leased_streams: usize,
    pub streams: Vec<RecordingDemandStreamHealth>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RecordingDemandStreamHealth {
    pub stream_id: String,
    pub viewers: usize,
    pub lease_remaining_ms: Option<u64>,
}

#[derive(Clone)]
pub struct RecordingDemand {
    inner: Arc<Inner>,
}

struct Inner {
    inactivity_grace: Duration,
    streams: Mutex<HashMap<String, StreamDemand>>,
}

#[derive(Default)]
struct StreamDemand {
    viewers: usize,
    lease_until: Option<Instant>,
}

#[must_use = "dropping the guard releases the viewer demand"]
pub struct RecordingDemandGuard {
    demand: RecordingDemand,
    stream_id: String,
    active: bool,
}

impl RecordingDemand {
    pub fn new(inactivity_grace: Duration) -> Self {
        Self {
            inner: Arc::new(Inner {
                inactivity_grace,
                streams: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn acquire(&self, stream_id: impl Into<String>) -> RecordingDemandGuard {
        let stream_id = stream_id.into();
        assert!(
            !stream_id.is_empty(),
            "recording demand stream ID must not be empty"
        );
        let mut streams = self
            .inner
            .streams
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stream = streams.entry(stream_id.clone()).or_default();
        stream.viewers = stream
            .viewers
            .checked_add(1)
            .expect("recording demand viewer count overflowed");
        drop(streams);

        RecordingDemandGuard {
            demand: self.clone(),
            stream_id,
            active: true,
        }
    }

    pub fn renew(&self, stream_id: &str, ttl: Duration) {
        self.renew_at(stream_id, ttl, Instant::now());
    }

    pub fn is_active(&self, stream_id: &str) -> bool {
        self.is_active_at(stream_id, Instant::now())
    }

    pub fn viewer_count(&self, stream_id: &str) -> usize {
        self.inner
            .streams
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(stream_id)
            .map_or(0, |stream| stream.viewers)
    }

    pub(crate) fn health_snapshot(&self) -> RecordingDemandHealth {
        let now = Instant::now();
        let mut streams = self
            .inner
            .streams
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        streams.retain(|_, stream| {
            stream.viewers > 0 || stream.lease_until.is_some_and(|until| until > now)
        });
        let mut reports = streams
            .iter()
            .map(|(stream_id, stream)| RecordingDemandStreamHealth {
                stream_id: stream_id.clone(),
                viewers: stream.viewers,
                lease_remaining_ms: stream.lease_until.and_then(|until| {
                    until
                        .checked_duration_since(now)
                        .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
                }),
            })
            .collect::<Vec<_>>();
        reports.sort_unstable_by(|left, right| left.stream_id.cmp(&right.stream_id));
        RecordingDemandHealth {
            active_streams: reports.len(),
            total_viewers: reports.iter().map(|stream| stream.viewers).sum(),
            leased_streams: reports
                .iter()
                .filter(|stream| stream.lease_remaining_ms.is_some())
                .count(),
            streams: reports,
        }
    }

    fn renew_at(&self, stream_id: &str, ttl: Duration, now: Instant) {
        assert!(
            !stream_id.is_empty(),
            "recording demand stream ID must not be empty"
        );
        let lease_until = now
            .checked_add(ttl)
            .expect("recording demand lease duration exceeds Instant range");
        let mut streams = self
            .inner
            .streams
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stream = streams.entry(stream_id.to_owned()).or_default();
        if stream
            .lease_until
            .is_none_or(|current| lease_until > current)
        {
            stream.lease_until = Some(lease_until);
        }
    }

    fn is_active_at(&self, stream_id: &str, now: Instant) -> bool {
        let mut streams = self
            .inner
            .streams
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(stream) = streams.get(stream_id) else {
            return false;
        };
        let active = stream.viewers > 0 || stream.lease_until.is_some_and(|until| until > now);
        if !active {
            streams.remove(stream_id);
        }
        active
    }

    fn release_at(&self, stream_id: &str, now: Instant) {
        let mut streams = self
            .inner
            .streams
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(stream) = streams.get_mut(stream_id) else {
            return;
        };
        assert!(
            stream.viewers > 0,
            "recording demand guard released without an active viewer"
        );
        stream.viewers -= 1;
        if stream.viewers == 0 && !self.inner.inactivity_grace.is_zero() {
            let grace_until = now
                .checked_add(self.inner.inactivity_grace)
                .expect("recording demand inactivity grace exceeds Instant range");
            if stream
                .lease_until
                .is_none_or(|current| grace_until > current)
            {
                stream.lease_until = Some(grace_until);
            }
        }
        let remove = stream.viewers == 0 && stream.lease_until.is_none_or(|until| until <= now);
        if remove {
            streams.remove(stream_id);
        }
    }
}

impl RecordingDemandGuard {
    #[cfg(test)]
    fn release_at(mut self, now: Instant) {
        self.demand.release_at(&self.stream_id, now);
        self.active = false;
    }
}

impl Drop for RecordingDemandGuard {
    fn drop(&mut self) {
        if self.active {
            self.demand.release_at(&self.stream_id, Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STREAM: &str = "front-door/main";

    #[test]
    fn guard_tracks_each_viewer() {
        let demand = RecordingDemand::new(Duration::ZERO);
        let first = demand.acquire(STREAM);
        let second = demand.acquire(STREAM);
        assert!(demand.is_active(STREAM));
        assert_eq!(demand.viewer_count(STREAM), 2);

        drop(first);
        assert!(demand.is_active(STREAM));
        assert_eq!(demand.viewer_count(STREAM), 1);

        drop(second);
        assert!(!demand.is_active(STREAM));
        assert_eq!(demand.viewer_count(STREAM), 0);
    }

    #[test]
    fn last_viewer_starts_inactivity_grace() {
        let grace = Duration::from_secs(30);
        let demand = RecordingDemand::new(grace);
        let now = Instant::now();
        let guard = demand.acquire(STREAM);

        guard.release_at(now);

        assert!(demand.is_active_at(STREAM, now + grace - Duration::from_millis(1)));
        assert!(!demand.is_active_at(STREAM, now + grace));
    }

    #[test]
    fn renewing_a_lease_never_shortens_it() {
        let demand = RecordingDemand::new(Duration::ZERO);
        let now = Instant::now();
        demand.renew_at(STREAM, Duration::from_secs(30), now);
        demand.renew_at(STREAM, Duration::from_secs(5), now);

        assert!(demand.is_active_at(STREAM, now + Duration::from_secs(29)));
        assert!(!demand.is_active_at(STREAM, now + Duration::from_secs(30)));
    }

    #[test]
    fn activity_is_scoped_to_one_recording_stream() {
        let demand = RecordingDemand::new(Duration::ZERO);
        let _guard = demand.acquire(STREAM);

        assert!(demand.is_active(STREAM));
        assert!(!demand.is_active("back-yard/main"));
    }
}
