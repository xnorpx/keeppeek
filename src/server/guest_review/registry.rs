use super::limits::{Action, Budget};
use crate::{
    shares::{BoundSession, ExportBinding, MediaDeadline, SessionContext},
    shutdown::Shutdown,
};
use rouille::ResponseAbortHandle;
use std::{
    collections::HashMap,
    io,
    net::IpAddr,
    sync::{
        Arc, Mutex, MutexGuard, Weak,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use uuid::Uuid;

const MAX_TRANSFERS: usize = 8;
const MAX_SESSION_TRANSFERS: usize = 2;
const MAX_HASH_READERS: usize = 2;
const WATCHDOG_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone)]
struct Binding {
    session_id: Uuid,
    grant_id: Uuid,
    revision: u64,
    evidence: ExportBinding,
    context: SessionContext,
}

#[derive(Clone)]
struct Flow {
    id: Uuid,
    binding: Option<Binding>,
    deadline: MediaDeadline,
    abort: ResponseAbortHandle,
    cancelled: Arc<AtomicBool>,
}
struct State {
    budget: Budget,
    flows: HashMap<Uuid, Flow>,
    hash_readers: usize,
    watchdog: bool,
}

pub struct Registry {
    state: Mutex<State>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            state: Mutex::new(State {
                budget: Budget::new(Instant::now()),
                flows: HashMap::new(),
                hash_readers: 0,
                watchdog: false,
            }),
        }
    }
}

impl Registry {
    pub fn start_deadline_watchdog(
        self: &Arc<Self>,
        shutdown: Shutdown,
    ) -> io::Result<JoinHandle<()>> {
        let mut state = self.lock()?;
        if state.watchdog {
            return Err(io::Error::other(
                "guest deadline watchdog is already active",
            ));
        }
        state.watchdog = true;
        let registry = Arc::downgrade(self);
        let spawned = thread::Builder::new()
            .name("guest-review-expiry".into())
            .spawn(move || {
                deadline_loop(registry, shutdown);
            });
        if spawned.is_err() {
            state.watchdog = false;
        }
        spawned
    }

    pub(super) fn admit_request(
        self: &Arc<Self>,
        peer: IpAddr,
        action: Action,
        abort: ResponseAbortHandle,
    ) -> io::Result<RequestGuard> {
        abort.enable_bounded_writes()?;
        self.reap_deadlines()?;
        let now = Instant::now();
        let wall = i64::try_from(super::super::unix_time_ms())
            .map_err(|_| io::Error::other("guest time unavailable"))?;
        let deadline = MediaDeadline {
            expires_at_ms: wall
                .checked_add(5_000)
                .ok_or_else(|| io::Error::other("guest time unavailable"))?,
            monotonic: now
                .checked_add(Duration::from_secs(5))
                .ok_or_else(|| io::Error::other("guest time unavailable"))?,
        };
        let mut state = self.lock()?;
        if !state.watchdog {
            return Err(io::Error::other("guest cancellation is unavailable"));
        }
        state.budget.admit(peer, action, now).map_err(|_| {
            io::Error::new(io::ErrorKind::WouldBlock, "guest request budget reached")
        })?;
        if state.flows.len() >= MAX_TRANSFERS {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "guest request capacity reached",
            ));
        }
        let id = Uuid::new_v4();
        let cancelled = Arc::new(AtomicBool::new(false));
        let flow = Flow {
            id,
            binding: None,
            deadline,
            abort,
            cancelled: Arc::clone(&cancelled),
        };
        assert!(
            state.flows.insert(id, flow).is_none(),
            "guest flow UUID collision"
        );
        Ok(RequestGuard {
            registry: Arc::clone(self),
            id,
            deadline,
            cancelled,
        })
    }
    pub(super) fn reserve_hash(self: &Arc<Self>) -> io::Result<HashPermit> {
        let mut state = self.lock()?;
        if state.hash_readers >= MAX_HASH_READERS {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "guest verification capacity reached",
            ));
        }
        state.hash_readers += 1;
        Ok(HashPermit(Arc::clone(self)))
    }

    pub fn abort_grants(&self, ids: &[Uuid]) {
        self.cancel_matching(|flow| {
            flow.binding
                .as_ref()
                .is_some_and(|binding| ids.contains(&binding.grant_id))
        });
    }

    pub fn abort_session(&self, id: Uuid) {
        self.cancel_matching(|flow| {
            flow.binding
                .as_ref()
                .is_some_and(|binding| binding.session_id == id)
        });
    }

    pub fn abort_all(&self) {
        self.cancel_matching(|_| true);
    }

    fn cancel_matching(&self, selected: impl Fn(&Flow) -> bool) {
        let flows = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state
                .flows
                .values()
                .filter(|flow| selected(flow))
                .cloned()
                .collect::<Vec<_>>()
        };
        for flow in flows {
            cancel(&flow);
        }
    }

    pub fn stop(&self) {
        let flows = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.watchdog = false;
            state.flows.values().cloned().collect::<Vec<_>>()
        };
        for flow in flows {
            cancel(&flow);
        }
    }
    fn lock(&self) -> io::Result<MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| io::Error::other("guest transfer registry is unavailable"))
    }

    fn reap_deadlines(&self) -> io::Result<()> {
        let now = Instant::now();
        let wall_ms = i64::try_from(super::super::unix_time_ms()).unwrap_or(i64::MAX);
        let mut cancelled = Vec::new();
        {
            let mut state = self.lock()?;
            state.flows.retain(|_, flow| {
                match flow.abort.is_finished() {
                    Ok(true) => return false,
                    Ok(false) => {}
                    Err(_) => cancelled.push(flow.clone()),
                }
                if now >= flow.deadline.monotonic || wall_ms >= flow.deadline.expires_at_ms {
                    cancelled.push(flow.clone());
                }
                true
            });
        }
        for flow in cancelled {
            cancel(&flow);
        }
        Ok(())
    }

    /// Live validation runs separately from the raw deadline thread and outside this registry lock.
    pub fn sweep(&self, state: &super::super::ServerState) {
        let Ok(flows) = self
            .lock()
            .map(|owner| owner.flows.values().cloned().collect::<Vec<_>>())
        else {
            self.abort_all();
            return;
        };
        for flow in flows {
            if !live(state, &flow) {
                cancel(&flow);
            }
        }
    }
}

fn live(state: &super::super::ServerState, flow: &Flow) -> bool {
    let Some(flow) = flow.binding.as_ref() else {
        return true;
    };
    use super::super::sharing;
    if !sharing::context(state).is_ok_and(|context| context == flow.context) {
        return false;
    }
    let record = sharing::with_store(state, |store, clock| {
        let grant = store
            .get(flow.grant_id, clock)?
            .ok_or(crate::shares::Error::Denied)?;
        if grant.revision != flow.revision
            || grant.revoked_at_ms.is_some()
            || clock.unix_ms >= grant.intent.expires_at_ms
            || grant.intent.binding != flow.evidence
        {
            return Err(crate::shares::Error::Denied);
        }
        Ok(grant)
    });
    record.is_ok_and(|grant| sharing::validate_guest(state, &grant).is_ok())
}

fn cancel(flow: &Flow) {
    if !flow.cancelled.swap(true, Ordering::AcqRel) && flow.abort.abort().is_err() {
        tracing::warn!("guest response transport cancellation failed");
    }
}

fn deadline_loop(registry: Weak<Registry>, shutdown: Shutdown) {
    let _stopped = StopOnDrop(registry.clone());
    while !shutdown.is_cancelled() {
        let Some(owner) = registry.upgrade() else {
            return;
        };
        if owner.reap_deadlines().is_err() {
            owner.abort_all();
            break;
        }
        drop(owner);
        if shutdown.wait_timeout(WATCHDOG_INTERVAL) {
            break;
        }
    }
}

struct StopOnDrop(Weak<Registry>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        if let Some(owner) = self.0.upgrade() {
            owner.stop();
        }
    }
}
pub(super) struct RequestGuard {
    registry: Arc<Registry>,
    id: Uuid,
    deadline: MediaDeadline,
    cancelled: Arc<AtomicBool>,
}

impl RequestGuard {
    pub fn check(&self) -> io::Result<()> {
        check(self.deadline, &self.cancelled)
    }

    pub fn bind(self, bound: &BoundSession, context: &SessionContext) -> io::Result<TransferGuard> {
        self.check()?;
        if !bound.media_active {
            return Err(io::Error::other("guest media expired"));
        }
        let deadline = bound
            .media_deadline
            .ok_or_else(|| io::Error::other("guest media expired"))?;
        {
            let mut state = self.registry.lock()?;
            if !state.watchdog || self.cancelled.load(Ordering::Acquire) {
                return Err(io::Error::other("guest cancellation is unavailable"));
            }
            let active = state
                .flows
                .values()
                .filter(|flow| {
                    flow.binding
                        .as_ref()
                        .is_some_and(|binding| binding.session_id == bound.session_id)
                })
                .count();
            if active >= MAX_SESSION_TRANSFERS {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "guest session transfer capacity reached",
                ));
            }
            let flow = state
                .flows
                .get_mut(&self.id)
                .ok_or_else(|| io::Error::other("guest request ended"))?;
            flow.deadline = deadline;
            flow.binding = Some(Binding {
                session_id: bound.session_id,
                grant_id: bound.grant.id,
                revision: bound.grant.revision,
                evidence: bound.grant.intent.binding.clone(),
                context: context.clone(),
            });
        }
        Ok(TransferGuard {
            registry: self.registry,
            id: self.id,
            deadline,
            cancelled: self.cancelled,
        })
    }
}

fn check(deadline: MediaDeadline, cancelled: &AtomicBool) -> io::Result<()> {
    let wall_ms = i64::try_from(super::super::unix_time_ms()).unwrap_or(i64::MAX);
    if cancelled.load(Ordering::Acquire)
        || Instant::now() >= deadline.monotonic
        || wall_ms >= deadline.expires_at_ms
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "guest transfer ended",
        ));
    }
    Ok(())
}
// The registry retains cancellation after this reader guard drops, through the writer's final flush.

pub(super) struct TransferGuard {
    registry: Arc<Registry>,
    id: Uuid,
    deadline: MediaDeadline,
    cancelled: Arc<AtomicBool>,
}

impl TransferGuard {
    pub fn check(&self) -> io::Result<()> {
        check(self.deadline, &self.cancelled)
    }

    pub fn pace(&self, peer: IpAddr, bytes: usize) -> io::Result<()> {
        let bytes =
            u64::try_from(bytes).map_err(|_| io::Error::other("guest read is too large"))?;
        loop {
            self.check()?;
            let wait = self
                .registry
                .lock()?
                .budget
                .egress_delay(peer, bytes, Instant::now())
                .map_err(|_| io::Error::other("guest egress is unavailable"))?;
            if wait.is_zero() {
                return Ok(());
            }
            thread::sleep(wait.min(WATCHDOG_INTERVAL));
        }
    }

    pub fn abort(&self) {
        self.registry.cancel_matching(|flow| flow.id == self.id);
    }
}

pub(super) struct HashPermit(Arc<Registry>);

impl Drop for HashPermit {
    fn drop(&mut self) {
        match self.0.lock() {
            Ok(mut state) => {
                assert!(state.hash_readers > 0, "guest hash permit underflow");
                state.hash_readers -= 1;
            }
            Err(_) => self.0.abort_all(),
        }
    }
}

#[cfg(test)]
#[path = "registry_tests.rs"]
pub(super) mod tests;
