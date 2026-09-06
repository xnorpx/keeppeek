use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::SyncSender,
};
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::{KeepPeekEvent, KeepPeekLoop};

const OWNERS_MAX: usize = 1024;
const EVENTS_PER_OWNER_MAX: usize = 128;
/// Limits retirement work on latency-sensitive periodic ticks.
const RETIRED_BATCH_MAX: usize = 64;
/// Gives healthy retirement bursts time to finish without unbounded shutdown retries.
const FINAL_DRAIN_BUDGET: Duration = Duration::from_secs(5);

#[derive(Default)]
pub(super) struct Commits {
    owners: HashMap<Uuid, Owner>,
}

struct Owner {
    lifetime: Arc<AtomicBool>,
    active: HashMap<String, i64>,
}

impl Commits {
    fn admit(&mut self, owner: Uuid, lifetime: &Arc<AtomicBool>) -> bool {
        if !lifetime.load(Ordering::Acquire) {
            return false;
        }
        if self.owners.len() >= OWNERS_MAX && !self.owners.contains_key(&owner) {
            return false;
        }
        let state = self.owners.entry(owner).or_insert_with(|| Owner {
            lifetime: Arc::clone(lifetime),
            active: HashMap::new(),
        });
        Arc::ptr_eq(&state.lifetime, lifetime)
    }

    fn has_capacity(&self, owner: Uuid, change: &KeepPeekEvent) -> bool {
        let state = &self.owners[&owner];
        match change {
            KeepPeekEvent::TimelineEventStarted { event } if event.end_time_ms.is_none() => {
                state.active.len() < EVENTS_PER_OWNER_MAX || state.active.contains_key(&event.id)
            }
            _ => true,
        }
    }

    fn note_ending(&mut self, owner: Uuid, change: &KeepPeekEvent) {
        if let KeepPeekEvent::TimelineEventEnded { id, end_time_ms } = change
            && let Some(state) = self.owners.get_mut(&owner)
            && let Some(time) = state.active.get_mut(id)
        {
            *time = (*time).max(*end_time_ms);
        }
    }

    fn record(&mut self, owner: Uuid, change: &KeepPeekEvent) {
        let active = &mut self
            .owners
            .get_mut(&owner)
            .expect("native owner was admitted")
            .active;
        match change {
            KeepPeekEvent::TimelineEventStarted { event } if event.end_time_ms.is_none() => {
                active.insert(event.id.clone(), event.start_time_ms);
            }
            KeepPeekEvent::TimelineEventImages { event, .. } => {
                if let Some(time) = active.get_mut(&event.id) {
                    let observed = event
                        .payload
                        .as_ref()
                        .and_then(|payload| payload.get("observation_time_ms"))
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(event.start_time_ms);
                    *time = (*time).max(observed);
                }
            }
            KeepPeekEvent::TimelineEventEnded { id, .. } => {
                active.remove(id);
            }
            _ => {}
        }
    }

    fn retired(&self) -> Vec<(Uuid, String, i64)> {
        self.owners
            .iter()
            .filter(|(_, owner)| !owner.lifetime.load(Ordering::Acquire))
            .flat_map(|(id, owner)| {
                owner
                    .active
                    .iter()
                    .map(move |(event_id, time)| (*id, event_id.clone(), *time))
            })
            .take(RETIRED_BATCH_MAX)
            .collect()
    }

    fn retired_count(&self) -> usize {
        self.owners
            .values()
            .filter(|owner| !owner.lifetime.load(Ordering::Acquire))
            .map(|owner| owner.active.len())
            .sum()
    }

    fn prune(&mut self) {
        self.owners
            .retain(|_, owner| owner.lifetime.load(Ordering::Acquire) || !owner.active.is_empty());
    }
}

impl KeepPeekLoop {
    pub(super) fn commit_native_batch(
        &mut self,
        owner: Uuid,
        lifetime: Arc<AtomicBool>,
        changes: Vec<KeepPeekEvent>,
        reply: SyncSender<usize>,
    ) {
        let mut completed = 0;
        if self.native_commits.admit(owner, &lifetime) {
            for change in changes {
                if !lifetime.load(Ordering::Acquire)
                    || !self.native_commits.has_capacity(owner, &change)
                {
                    break;
                }
                self.native_commits.note_ending(owner, &change);
                let optional = matches!(change, KeepPeekEvent::TimelineEventThumbnail { .. });
                match self.commit_isapi_change(change.clone()) {
                    Ok(()) => self.native_commits.record(owner, &change),
                    Err(_) if optional => tracing::warn!(
                        "optional native snapshot commit failed; lifecycle delivery continues"
                    ),
                    Err(error) => {
                        tracing::warn!(%error, completed, "native camera event commit paused");
                        break;
                    }
                }
                completed += 1;
            }
        }
        let _ = reply.send(completed);
        self.close_retired_native_events();
    }

    pub(super) fn close_retired_native_events(&mut self) {
        self.close_retired_native_events_until(None);
    }

    /// Returns the number of retired events that remain after best-effort shutdown.
    ///
    /// The budget cannot interrupt synchronous storage operations or callbacks.
    pub(super) fn drain_retired_native_events(&mut self) -> usize {
        self.drain_retired_native_events_until(Instant::now() + FINAL_DRAIN_BUDGET)
    }

    pub(super) fn drain_retired_native_events_until(&mut self, deadline: Instant) -> usize {
        let mut remaining = self.native_commits.retired_count();
        let attempts_max = remaining;
        for _ in 0..attempts_max {
            if remaining == 0 || Instant::now() >= deadline {
                break;
            }
            if self.close_retired_native_events_until(Some(deadline)) == 0 {
                break;
            }
            remaining = self.native_commits.retired_count();
        }
        if remaining != 0 {
            tracing::warn!(
                name: "native.shutdown.incomplete",
                remaining,
                deadline_reached = Instant::now() >= deadline,
                "final native event closure stopped with undrained events"
            );
        }
        remaining
    }

    fn close_retired_native_events_until(&mut self, deadline: Option<Instant>) -> usize {
        let mut completed = 0;
        for (owner, id, end_time_ms) in self.native_commits.retired() {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                break;
            }
            let change = KeepPeekEvent::TimelineEventEnded { id, end_time_ms };
            match self.commit_isapi_change(change.clone()) {
                Ok(()) => {
                    self.native_commits.record(owner, &change);
                    completed += 1;
                }
                Err(error) => {
                    tracing::warn!(%error, "retired native event closure remains pending");
                }
            }
        }
        self.native_commits.prune();
        completed
    }
}
