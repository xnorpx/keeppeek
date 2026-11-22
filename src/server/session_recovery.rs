//! Single-use privacy transport recovery. The request still authenticates independently.

use super::*;

pub(super) const HEADER: &str = "X-KeepPeek-Previous-Session";
// Allow one ICE failure detection interval plus a new offer. Bound abandoned
// continuations independently of the number of new signaling attempts.
const LIFETIME: Duration = Duration::from_secs(60);
const CAPACITY: usize = 256;

#[derive(Default)]
pub(super) struct Directory(Mutex<HashMap<SessionId, (Instant, ApiSessionRecord)>>);

pub(super) fn requested(request: &Request) -> Result<Option<SessionId>, Response> {
    let mut values = request
        .headers()
        .filter(|(name, _)| name.eq_ignore_ascii_case(HEADER));
    let Some((_, value)) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(api_status(400, "invalid previous session"));
    }
    value
        .parse::<u64>()
        .ok()
        .filter(|id| *id != 0 && id.to_string() == value)
        .map(|id| Some(SessionId::from_u64(id)))
        .ok_or_else(|| api_status(400, "invalid previous session"))
}

impl Directory {
    pub(super) fn retire(&self, state: &ServerState, id: SessionId) {
        let owners = state
            .api_session_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(session) = owners.get(&id) else {
            return;
        };
        if session.lifecycle.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let now = Instant::now();
        let mut entries = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.retain(|_, (when, _)| now.saturating_duration_since(*when) < LIFETIME);
        if entries.len() < CAPACITY {
            entries.insert(id, (now, session.clone()));
        }
    }

    pub(super) fn restore(
        &self,
        id: SessionId,
        next: &mut ApiSessionRecord,
        idle: Duration,
    ) -> bool {
        let mut entries = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some((when, previous)) = entries.get(&id) else {
            return false;
        };
        if !previous.principal.owns_session(&next.principal)
            || previous.classification.effective_address != next.classification.effective_address
        {
            return false;
        }
        if next.last_activity.saturating_duration_since(*when) >= LIFETIME
            || next.created_at_ms >= previous.absolute_expires_at_ms
            || next
                .last_activity
                .saturating_duration_since(previous.last_activity)
                >= idle
        {
            entries.remove(&id);
            return false;
        }
        next.created_at_ms = previous.created_at_ms;
        next.last_activity_at_ms = previous.last_activity_at_ms;
        next.last_activity = previous.last_activity;
        next.absolute_expires_at_ms = next
            .absolute_expires_at_ms
            .min(previous.absolute_expires_at_ms);
        entries.remove(&id);
        true
    }

    pub(super) fn cancel(&self, id: SessionId, owner: Option<&AuthenticatedApiRequest>) {
        let mut entries = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if entries.get(&id).is_some_and(|(_, record)| {
            owner.is_none_or(|request| {
                record.principal.owns_session(&request.principal)
                    && record.classification.effective_address
                        == request.classification.effective_address
            })
        }) {
            entries.remove(&id);
        }
    }

    pub(super) fn prune(&self, now: Instant) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, (when, _)| now.saturating_duration_since(*when) < LIFETIME);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::tests::local_test_session;

    #[test]
    fn recovery_requires_the_same_identity_and_unexpired_original_clocks() {
        let state = ServerState::empty();
        let directory = &state.session_recovery;
        let id = SessionId::from_u64(9);
        for boundary in [
            "valid",
            "absolute",
            "idle",
            "retention",
            "cancelled",
            "closed",
        ] {
            let previous = local_test_session();
            let mut candidate = local_test_session();
            candidate.last_activity = previous.last_activity + Duration::from_secs(1);
            if boundary == "closed" {
                previous.lifecycle.closed.store(true, Ordering::Release);
            }
            state
                .api_session_owners
                .lock()
                .unwrap()
                .insert(id, previous.clone());
            directory.retire(&state, id);
            let mut other = candidate.clone();
            other.principal = ApiPrincipal::local("192.0.2.33".parse().unwrap());
            assert!(!directory.restore(id, &mut other, Duration::from_secs(30)));
            match boundary {
                "absolute" => candidate.created_at_ms = previous.absolute_expires_at_ms,
                "idle" => {
                    candidate.last_activity = previous.last_activity + Duration::from_secs(30)
                }
                "retention" => directory.prune(Instant::now() + LIFETIME),
                "cancelled" => directory.cancel(id, None),
                _ => {}
            }
            assert_eq!(
                directory.restore(id, &mut candidate, Duration::from_secs(30)),
                boundary == "valid"
            );
            if boundary == "valid" {
                assert_eq!(candidate.created_at_ms, previous.created_at_ms);
                assert_eq!(candidate.last_activity_at_ms, previous.last_activity_at_ms);
                assert_eq!(
                    candidate.absolute_expires_at_ms,
                    previous.absolute_expires_at_ms
                );
                assert_eq!(candidate.last_activity, previous.last_activity);
            }
            assert!(!directory.restore(id, &mut candidate, Duration::from_secs(30)));
        }
    }
}
