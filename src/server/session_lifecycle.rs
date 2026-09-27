//! Serializes work admission with teardown without holding the session-directory lock.

use super::*;

#[derive(Default)]
pub(super) struct Lifecycle {
    pub(super) closed: AtomicBool,
    admission: Mutex<()>,
}

pub(super) fn admit<T>(
    state: &ServerState,
    id: SessionId,
    register: impl FnOnce() -> Result<T, ControlCommandError>,
) -> Result<T, ControlCommandError> {
    if id.as_u64() == 0 {
        return register();
    }
    let session = lookup(state, id).ok_or_else(unavailable)?;
    if session.lifecycle.closed.load(Ordering::Acquire) {
        return Err(unavailable());
    }
    let _admission = match session.lifecycle.admission.try_lock() {
        Ok(guard) => guard,
        Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => {
            return Err(ControlCommandError::new(
                proto::ErrorCode::Rejected,
                409,
                "API session admission is busy",
            ));
        }
    };
    let now = Instant::now();
    let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    if session.lifecycle.closed.load(Ordering::Acquire)
        || now_ms >= session.absolute_expires_at_ms
        || now.saturating_duration_since(session.last_activity)
            >= state.api_session_policy.idle_timeout
        || !authentication::active(state, &session.principal, now, now_ms)
    {
        return Err(unavailable());
    }
    register()
}

fn unavailable() -> ControlCommandError {
    ControlCommandError::new(
        proto::ErrorCode::Rejected,
        401,
        "API session is unavailable",
    )
}

fn lookup(state: &ServerState, id: SessionId) -> Option<ApiSessionRecord> {
    state
        .api_session_owners
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&id)
        .cloned()
}

pub(super) fn close(state: &ServerState, id: SessionId) -> Option<ApiSessionRecord> {
    let session = lookup(state, id)?;
    session.lifecycle.closed.store(true, Ordering::Release);
    let _admission = session
        .lifecycle
        .admission
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    lookup(state, id)?;
    // The owner remains visible until cleanup finishes so concurrent revokers wait for it.
    state.talkback.stop_for_session(id);
    state.webrtc.disarm_talkback(id);
    event_search::close_session(state, id);
    state.state_store_watches.close_session(id);
    state.event_publications.close_session(id);
    state.event_subscriptions.close_session(id);
    state.camera_discovery_tasks.close_session(id);
    state
        .configuration_plans
        .proofs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .close_session(id);
    state
        .configuration_plans
        .restore_proofs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .close_session(id);
    stored_media::close_session(state, id);
    camera_control::close_session(state, id);
    state
        .api_session_owners
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&id)
}
