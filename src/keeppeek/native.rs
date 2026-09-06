use std::collections::{HashMap, VecDeque};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::SyncSender,
};
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::{KeepPeekEvent, KeepPeekLoop, TimelineEvent, Trigger};

const OWNERS_MAX: usize = 1024;
const EVENTS_PER_OWNER_MAX: usize = 128;
/// Limits retirement work on latency-sensitive periodic ticks.
const RETIRED_BATCH_MAX: usize = crate::storage::catalog::NATIVE_EVENT_CLOSE_BATCH_MAX;
/// Gives healthy retirement bursts time to finish without unbounded shutdown retries.
const FINAL_DRAIN_BUDGET: Duration = Duration::from_secs(5);

#[derive(Default)]
pub(super) struct Commits {
    owners: HashMap<Uuid, Owner>,
    pending: VecDeque<Retired>,
}

struct Retired {
    owner: Uuid,
    change: KeepPeekEvent,
    event: Option<TimelineEvent>,
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
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return 0;
        }
        if let Err(error) = self.stage_retired_native_events() {
            tracing::warn!(%error, "retired native event closure remains pending");
            return 0;
        }
        let mut completed = 0;
        for _ in 0..RETIRED_BATCH_MAX {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                break;
            }
            let Some(pending) = self.native_commits.pending.front() else {
                break;
            };
            if let Some(event) = &pending.event
                && let Err(error) = self.publish_native_revision(event, Trigger::EventEnded)
            {
                tracing::warn!(%error, "committed native ending publication remains pending");
                break;
            }
            let pending = self
                .native_commits
                .pending
                .pop_front()
                .expect("pending ending was just inspected");
            self.native_commits.record(pending.owner, &pending.change);
            completed += 1;
        }
        self.native_commits.prune();
        completed
    }

    fn stage_retired_native_events(&mut self) -> anyhow::Result<()> {
        if !self.native_commits.pending.is_empty() {
            return Ok(());
        }
        let retired = self.native_commits.retired();
        if retired.is_empty() {
            return Ok(());
        }
        let events = self
            .events
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("native event storage is unavailable"))?;
        let endings = retired
            .iter()
            .map(|(_, id, end_time_ms)| (id.clone(), *end_time_ms))
            .collect::<Vec<_>>();
        let committed = events.close_native_events(&endings)?;
        assert_eq!(
            committed.len(),
            retired.len(),
            "every requested ending must have a commit result"
        );
        self.native_commits
            .pending
            .extend(
                retired
                    .into_iter()
                    .zip(committed)
                    .map(|((owner, id, end_time_ms), event)| Retired {
                        owner,
                        change: KeepPeekEvent::TimelineEventEnded { id, end_time_ms },
                        event,
                    }),
            );
        assert!(
            self.native_commits.pending.len() <= RETIRED_BATCH_MAX,
            "retired publication batch exceeded its bound"
        );
        Ok(())
    }
}
