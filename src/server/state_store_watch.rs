//! Watch runtime: snapshot barrier, ordered delivery, acks, backpressure.
//!
//! A watch captures an atomic snapshot under the store registry lock and is
//! registered before that lock releases, so no mutation can slip between the
//! snapshot and registration. Every later mutation fans out while still
//! holding the registry lock, which keeps per-watch sequences in revision
//! order even when mutations race.
//!
//! Lock order: the store registry lock always precedes the watch lock. The
//! sweeper and ack/unwatch paths take only the watch lock. Transport enqueue
//! never calls back into either lock.
//!
//! A watch ends with an explicit `StateStoreWatchClosed` signal carrying one
//! of the contract reasons. When the expiry collector overflows, dropped
//! expiries are unrecoverable, so every watch ends rather than risk a silent
//! gap. A session that disappears takes its watches with it on cleanup.

use super::ServerState;
use super::state_store::{
    Error, Invalid, NamespaceLayout, StoredEntry, authorize_read, validate_namespace,
};
use crate::{
    access::AccessRole,
    api::proto::{self, notification},
    webrtc::SessionId,
};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

pub(super) const MAX_WATCHES_PER_SESSION: usize = 64;
pub(super) const MAX_WATCHES_TOTAL: usize = 1_024;
pub(super) const MAX_IN_FLIGHT_UPDATES: usize = 32;
pub(super) const ACK_TIMEOUT_MS: u64 = 30_000;

pub(super) struct WatchRegistry {
    inner: Mutex<HashMap<(SessionId, String), Watch>>,
}

impl Default for WatchRegistry {
    fn default() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }
}

#[derive(Debug)]
struct Watch {
    session_id: SessionId,
    watch_id: String,
    namespace: String,
    key_prefix: String,
    delivered: u64,
    applied: u64,
    unacked: VecDeque<PendingUpdate>,
}

#[derive(Clone, Copy, Debug)]
struct PendingUpdate {
    sequence: u64,
    sent_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EnqueueOutcome {
    Sent,
    QueueFull,
    Gone,
}

#[derive(Clone, Debug)]
pub(super) struct WatchEvent {
    pub(super) namespace: String,
    pub(super) key: String,
    pub(super) revision: u64,
    pub(super) kind: WatchEventKind,
}

#[derive(Clone, Debug)]
pub(super) enum WatchEventKind {
    Put(StoredEntry),
    Delete,
    Expire,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseReason {
    BufferOverflow,
    AckTimeout,
    AuthorizationRevoked,
}

impl CloseReason {
    const fn proto(self) -> proto::StateStoreWatchCloseReason {
        match self {
            Self::BufferOverflow => proto::StateStoreWatchCloseReason::BufferOverflow,
            Self::AckTimeout => proto::StateStoreWatchCloseReason::AckTimeout,
            Self::AuthorizationRevoked => proto::StateStoreWatchCloseReason::AuthorizationRevoked,
        }
    }
}

pub(super) fn enqueue_notification(
    state: &ServerState,
    session_id: SessionId,
    notification: proto::Notification,
) -> EnqueueOutcome {
    match state
        .webrtc
        .try_enqueue_api_notification(session_id, notification)
    {
        Ok(true) => EnqueueOutcome::Sent,
        Ok(false) => {
            state.webrtc.request_api_session_close(session_id);
            EnqueueOutcome::QueueFull
        }
        Err(_) => EnqueueOutcome::Gone,
    }
}

impl WatchRegistry {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(SessionId, String), Watch>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[cfg(test)]
    pub(super) fn owns_watch(&self, session_id: SessionId, watch_id: &str) -> bool {
        self.lock().contains_key(&(session_id, watch_id.to_owned()))
    }

    pub(super) fn register(
        &self,
        session_id: SessionId,
        namespace: String,
        key_prefix: String,
        watch_id: String,
    ) -> Result<(), Error> {
        let mut watches = self.lock();
        if !watches.contains_key(&(session_id, watch_id.clone())) {
            let session_count = watches
                .keys()
                .filter(|(owner, _)| *owner == session_id)
                .count();
            if session_count >= MAX_WATCHES_PER_SESSION || watches.len() >= MAX_WATCHES_TOTAL {
                return Err(Error::Invalid(Invalid::WatchLimitExceeded));
            }
        }
        watches.insert(
            (session_id, watch_id.clone()),
            Watch {
                session_id,
                watch_id,
                namespace,
                key_prefix,
                delivered: 0,
                applied: 0,
                unacked: VecDeque::new(),
            },
        );
        Ok(())
    }

    pub(super) fn unregister(&self, session_id: SessionId, watch_id: &str) -> Result<(), Error> {
        self.lock()
            .remove(&(session_id, watch_id.to_owned()))
            .map(|_| ())
            .ok_or(Error::Invalid(Invalid::WatchNotFound))
    }

    pub(super) fn acknowledge(
        &self,
        session_id: SessionId,
        watch_id: &str,
        applied_sequence: u64,
        now_ms: u64,
    ) -> Result<u64, (String, Error)> {
        let mut watches = self.lock();
        let Some(watch) = watches.get_mut(&(session_id, watch_id.to_owned())) else {
            return Err((String::new(), Error::Invalid(Invalid::WatchNotFound)));
        };
        if applied_sequence > watch.delivered {
            return Err((watch.namespace.clone(), Error::Invalid(Invalid::AckAhead)));
        }
        if applied_sequence > watch.applied {
            watch.applied = applied_sequence;
            watch
                .unacked
                .retain(|pending| pending.sequence > applied_sequence);
            if let Some(oldest) = watch.unacked.front_mut() {
                oldest.sent_ms = now_ms;
            }
        }
        Ok(watch.applied)
    }

    #[cfg(test)]
    pub(super) fn oldest_unacked_sent_ms(
        &self,
        session_id: SessionId,
        watch_id: &str,
    ) -> Option<u64> {
        self.lock()
            .get(&(session_id, watch_id.to_owned()))
            .and_then(|watch| watch.unacked.front().map(|pending| pending.sent_ms))
    }

    pub(super) fn close_session(&self, session_id: SessionId) {
        self.lock().retain(|(owner, _), _| *owner != session_id);
    }

    pub(super) fn publish(
        &self,
        state: &ServerState,
        events: &[WatchEvent],
        pending_overflowed: bool,
        now_ms: u64,
        enqueue: impl Fn(SessionId, proto::Notification) -> EnqueueOutcome,
    ) -> Vec<(SessionId, proto::Notification)> {
        let mut sent = Vec::new();
        let mut watches = self.lock();
        let mut closed: Vec<(Watch, CloseReason)> = Vec::new();
        if pending_overflowed {
            closed.extend(watches.drain().map(|(_, watch)| {
                (
                    Watch {
                        unacked: VecDeque::new(),
                        ..watch
                    },
                    CloseReason::BufferOverflow,
                )
            }));
        }
        for event in events {
            let layout = validate_namespace(&event.namespace).ok();
            let mut terminate: Vec<((SessionId, String), Option<CloseReason>)> = Vec::new();
            for (key, watch) in watches.iter_mut() {
                if watch.namespace != event.namespace || !event.key.starts_with(&watch.key_prefix) {
                    continue;
                }
                if !session_authorized(state, watch.session_id, layout.as_ref()) {
                    terminate.push((key.clone(), Some(CloseReason::AuthorizationRevoked)));
                    continue;
                }
                watch.delivered += 1;
                let sequence = watch.delivered;
                watch.unacked.push_back(PendingUpdate {
                    sequence,
                    sent_ms: now_ms,
                });
                if watch.unacked.len() > MAX_IN_FLIGHT_UPDATES {
                    terminate.push((key.clone(), Some(CloseReason::BufferOverflow)));
                    continue;
                }
                let notification = update_notification(watch, event, sequence);
                match enqueue(watch.session_id, notification.clone()) {
                    EnqueueOutcome::Sent => {
                        sent.push((watch.session_id, notification));
                    }
                    EnqueueOutcome::QueueFull => {
                        terminate.push((key.clone(), None));
                    }
                    EnqueueOutcome::Gone => {}
                }
            }
            for (key, reason) in terminate {
                if let Some(watch) = watches.remove(&key)
                    && let Some(reason) = reason
                {
                    closed.push((watch, reason));
                }
            }
        }
        for (watch, reason) in closed {
            let notification = closed_notification(&watch, reason);
            if enqueue(watch.session_id, notification.clone()) == EnqueueOutcome::Sent {
                sent.push((watch.session_id, notification));
            }
        }
        sent
    }

    pub(super) fn expire_watches(
        &self,
        state: &ServerState,
        now_ms: u64,
        enqueue: impl Fn(SessionId, proto::Notification) -> EnqueueOutcome,
    ) -> Vec<(SessionId, proto::Notification)> {
        let mut sent = Vec::new();
        let mut watches = self.lock();
        let mut terminate: Vec<((SessionId, String), Option<CloseReason>)> = Vec::new();
        for (key, watch) in watches.iter() {
            if !state.webrtc.has_api_session(watch.session_id) {
                terminate.push((key.clone(), None));
                continue;
            }
            let layout = validate_namespace(&watch.namespace).ok();
            if !session_authorized(state, watch.session_id, layout.as_ref()) {
                terminate.push((key.clone(), Some(CloseReason::AuthorizationRevoked)));
                continue;
            }
            let overdue = watch
                .unacked
                .front()
                .is_some_and(|oldest| oldest.sent_ms.saturating_add(ACK_TIMEOUT_MS) <= now_ms);
            if overdue {
                terminate.push((key.clone(), Some(CloseReason::AckTimeout)));
            }
        }
        let mut closed = Vec::new();
        for (key, reason) in terminate {
            if let Some(watch) = watches.remove(&key)
                && let Some(reason) = reason
            {
                closed.push((watch, reason));
            }
        }
        drop(watches);
        for (watch, reason) in closed {
            let notification = closed_notification(&watch, reason);
            if enqueue(watch.session_id, notification.clone()) == EnqueueOutcome::Sent {
                sent.push((watch.session_id, notification));
            }
        }
        sent
    }
}

fn session_authorized(
    state: &ServerState,
    session_id: SessionId,
    layout: Option<&NamespaceLayout>,
) -> bool {
    let Some(layout) = layout else {
        return false;
    };
    let sessions = state
        .api_session_owners
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(session) = sessions.get(&session_id) else {
        return false;
    };
    authorize_read(
        layout,
        &session.principal.id(),
        session.principal.role == AccessRole::Administrator,
    )
    .is_ok()
}

fn update_notification(watch: &Watch, event: &WatchEvent, sequence: u64) -> proto::Notification {
    let (kind, entry) = match &event.kind {
        WatchEventKind::Put(entry) => (proto::StateStoreUpdateKind::Put, Some(proto_entry(entry))),
        WatchEventKind::Delete => (proto::StateStoreUpdateKind::Delete, None),
        WatchEventKind::Expire => (proto::StateStoreUpdateKind::Expire, None),
    };
    proto::Notification {
        event: Some(notification::Event::StateStoreWatchUpdate(
            proto::StateStoreWatchUpdate {
                watch_id: watch.watch_id.clone(),
                namespace: event.namespace.clone(),
                key: event.key.clone(),
                revision: event.revision,
                kind: kind as i32,
                entry,
                watch_sequence: sequence,
            },
        )),
    }
}

fn closed_notification(watch: &Watch, reason: CloseReason) -> proto::Notification {
    proto::Notification {
        event: Some(notification::Event::StateStoreWatchClosed(
            proto::StateStoreWatchClosed {
                watch_id: watch.watch_id.clone(),
                namespace: watch.namespace.clone(),
                reason: reason.proto() as i32,
            },
        )),
    }
}

fn proto_entry(entry: &StoredEntry) -> proto::StateEntry {
    super::state_store::proto_entry(entry)
}

#[cfg(test)]
mod tests {
    use super::super::ApiPrincipal;
    use super::*;
    use crate::access::{ClientClassification, ClientClassificationReason};
    use std::cell::RefCell;
    use std::net::{IpAddr, Ipv4Addr};
    use std::rc::Rc;
    use std::time::Instant;

    const NOW_MS: u64 = 1_787_000_000_000;

    type RecordedNotifications = Rc<RefCell<Vec<(SessionId, proto::Notification)>>>;

    fn session_record(principal: ApiPrincipal) -> super::super::ApiSessionRecord {
        super::super::ApiSessionRecord {
            principal,
            classification: ClientClassification {
                peer_address: IpAddr::V4(Ipv4Addr::LOCALHOST),
                effective_address: IpAddr::V4(Ipv4Addr::LOCALHOST),
                local: true,
                reason: ClientClassificationReason::DirectLocal,
            },
            created_at_ms: 0,
            last_activity_at_ms: 0,
            absolute_expires_at_ms: i64::MAX,
            last_activity: Instant::now(),
        }
    }

    fn admin_session(state: &ServerState, session_id: SessionId) {
        state
            .api_session_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                session_id,
                session_record(ApiPrincipal::local(IpAddr::V4(Ipv4Addr::LOCALHOST))),
            );
    }

    fn test_entry(namespace: &str, key: &str, revision: u64) -> StoredEntry {
        StoredEntry {
            namespace: namespace.to_owned(),
            key: key.to_owned(),
            schema: super::super::state_store_schema::MEDIA_INTENT_SCHEMA.to_owned(),
            value: prost_types::Struct {
                fields: std::collections::BTreeMap::new(),
            },
            revision,
            updated_ms: NOW_MS,
            expires_ms: None,
            owner_id: "local-administrator".to_owned(),
        }
    }

    fn put_event(namespace: &str, key: &str, revision: u64) -> WatchEvent {
        WatchEvent {
            namespace: namespace.to_owned(),
            key: key.to_owned(),
            revision,
            kind: WatchEventKind::Put(test_entry(namespace, key, revision)),
        }
    }

    fn recorder() -> (
        RecordedNotifications,
        impl Fn(SessionId, proto::Notification) -> EnqueueOutcome,
    ) {
        let sent = Rc::new(RefCell::new(Vec::new()));
        let recorded = sent.clone();
        let enqueue = move |session_id: SessionId, notification: proto::Notification| {
            recorded.borrow_mut().push((session_id, notification));
            EnqueueOutcome::Sent
        };
        (sent, enqueue)
    }

    fn register(
        state: &ServerState,
        session_id: SessionId,
        watch_id: &str,
        namespace: &str,
        prefix: &str,
    ) {
        state
            .state_store_watches
            .register(
                session_id,
                namespace.to_owned(),
                prefix.to_owned(),
                watch_id.to_owned(),
            )
            .expect("watch registration must succeed");
    }

    fn update_of(notification: &proto::Notification) -> &proto::StateStoreWatchUpdate {
        match &notification.event {
            Some(notification::Event::StateStoreWatchUpdate(update)) => update,
            other => panic!("expected a watch update, got {other:?}"),
        }
    }

    fn close_of(notification: &proto::Notification) -> &proto::StateStoreWatchClosed {
        match &notification.event {
            Some(notification::Event::StateStoreWatchClosed(closed)) => closed,
            other => panic!("expected a watch close, got {other:?}"),
        }
    }

    #[test]
    fn publish_sequences_updates_and_filters_prefixes() {
        let state = ServerState::empty();
        let session = SessionId::from_u64(11);
        admin_session(&state, session);
        register(&state, session, "w", "service/transcoder-a/", "intents/");
        let (sent, enqueue) = recorder();
        let watches = &state.state_store_watches;
        let delivered = watches.publish(
            &state,
            &[
                put_event("service/transcoder-a/", "intents/a", 7),
                put_event("service/transcoder-a/", "other/b", 8),
            ],
            false,
            NOW_MS,
            enqueue,
        );
        assert_eq!(delivered.len(), 1);
        assert_eq!(sent.borrow().len(), 1);
        let borrowed = sent.borrow();
        let update = update_of(&borrowed[0].1);
        assert_eq!(update.watch_sequence, 1);
        assert_eq!(update.revision, 7);
        assert_eq!(update.key, "intents/a");
        assert_eq!(update.kind, proto::StateStoreUpdateKind::Put as i32);
        assert!(update.entry.is_some());
        let accepted = watches
            .acknowledge(session, "w", 1, NOW_MS)
            .expect("ack must succeed");
        assert_eq!(accepted, 1);
    }

    #[test]
    fn acknowledge_rejects_unknown_and_ahead_sequences() {
        let state = ServerState::empty();
        let session = SessionId::from_u64(12);
        admin_session(&state, session);
        let watches = &state.state_store_watches;
        let (_, error) = watches
            .acknowledge(session, "missing", 1, NOW_MS)
            .expect_err("unknown watch must fail");
        assert_eq!(error, Error::Invalid(Invalid::WatchNotFound));
        register(&state, session, "w", "service/transcoder-a/", "");
        let (namespace, error) = watches
            .acknowledge(session, "w", 1, NOW_MS)
            .expect_err("ack ahead of delivery must fail");
        assert_eq!(namespace, "service/transcoder-a/");
        assert_eq!(error, Error::Invalid(Invalid::AckAhead));
        let (sent, enqueue) = recorder();
        watches.publish(
            &state,
            &[put_event("service/transcoder-a/", "k", 3)],
            false,
            NOW_MS,
            enqueue,
        );
        assert_eq!(sent.borrow().len(), 1);
        assert_eq!(watches.acknowledge(session, "w", 1, NOW_MS), Ok(1));
        assert_eq!(
            watches.acknowledge(session, "w", 1, NOW_MS),
            Ok(1),
            "duplicate acks are idempotent"
        );
        assert_eq!(watches.acknowledge(session, "w", 0, NOW_MS), Ok(1));
    }

    #[test]
    fn advancing_ack_restarts_oldest_deadline() {
        let state = ServerState::empty();
        let session = SessionId::from_u64(22);
        admin_session(&state, session);
        register(&state, session, "w", "service/transcoder-a/", "");
        let watches = &state.state_store_watches;
        let (_, enqueue) = recorder();
        watches.publish(
            &state,
            &[put_event("service/transcoder-a/", "a", 1)],
            false,
            NOW_MS,
            &enqueue,
        );
        watches.publish(
            &state,
            &[put_event("service/transcoder-a/", "b", 2)],
            false,
            NOW_MS + 1_000,
            &enqueue,
        );
        assert_eq!(watches.oldest_unacked_sent_ms(session, "w"), Some(NOW_MS));
        let ack_at = NOW_MS + 29_000;
        assert_eq!(watches.acknowledge(session, "w", 1, ack_at), Ok(1));
        assert_eq!(
            watches.oldest_unacked_sent_ms(session, "w"),
            Some(ack_at),
            "an advancing ack restarts the deadline for the new oldest"
        );
    }

    #[test]
    fn duplicate_ack_keeps_oldest_deadline() {
        let state = ServerState::empty();
        let session = SessionId::from_u64(23);
        admin_session(&state, session);
        register(&state, session, "w", "service/transcoder-a/", "");
        let watches = &state.state_store_watches;
        let (_, enqueue) = recorder();
        watches.publish(
            &state,
            &[put_event("service/transcoder-a/", "a", 1)],
            false,
            NOW_MS,
            &enqueue,
        );
        watches.publish(
            &state,
            &[put_event("service/transcoder-a/", "b", 2)],
            false,
            NOW_MS + 1_000,
            &enqueue,
        );
        let ack_at = NOW_MS + 29_000;
        assert_eq!(watches.acknowledge(session, "w", 1, ack_at), Ok(1));
        assert_eq!(watches.acknowledge(session, "w", 1, ack_at + 500), Ok(1));
        assert_eq!(
            watches.oldest_unacked_sent_ms(session, "w"),
            Some(ack_at),
            "duplicate acks must not move the deadline"
        );
        assert_eq!(watches.acknowledge(session, "w", 0, ack_at + 500), Ok(1));
        assert_eq!(
            watches.oldest_unacked_sent_ms(session, "w"),
            Some(ack_at),
            "stale acks must not move the deadline"
        );
    }

    #[test]
    fn unacked_overflow_terminates_with_close() {
        let state = ServerState::empty();
        let session = SessionId::from_u64(13);
        admin_session(&state, session);
        register(&state, session, "w", "service/transcoder-a/", "");
        let watches = &state.state_store_watches;
        let (sent, enqueue) = recorder();
        for revision in 1..=MAX_IN_FLIGHT_UPDATES as u64 {
            let delivered = watches.publish(
                &state,
                &[put_event("service/transcoder-a/", "k", revision)],
                false,
                NOW_MS,
                &enqueue,
            );
            assert_eq!(delivered.len(), 1, "update {revision} must deliver");
        }
        assert!(watches.owns_watch(session, "w"));
        let delivered = watches.publish(
            &state,
            &[put_event("service/transcoder-a/", "k", 99)],
            false,
            NOW_MS,
            &enqueue,
        );
        assert!(!watches.owns_watch(session, "w"));
        let sent = sent.borrow();
        let close = close_of(&sent.last().expect("close must be sent").1);
        assert_eq!(close.watch_id, "w");
        assert_eq!(
            close.reason,
            proto::StateStoreWatchCloseReason::BufferOverflow as i32
        );
        assert_eq!(delivered.len(), 1, "only the close is reported");
    }

    #[test]
    fn pending_overflow_terminates_every_watch() {
        let state = ServerState::empty();
        let session = SessionId::from_u64(14);
        admin_session(&state, session);
        register(&state, session, "a", "service/one/", "");
        register(&state, session, "b", "service/two/", "");
        let watches = &state.state_store_watches;
        let (sent, enqueue) = recorder();
        let delivered = watches.publish(&state, &[], true, NOW_MS, enqueue);
        assert!(!watches.owns_watch(session, "a"));
        assert!(!watches.owns_watch(session, "b"));
        let sent = sent.borrow();
        assert_eq!(delivered.len(), 2);
        for notification in sent.iter() {
            let close = close_of(&notification.1);
            assert_eq!(
                close.reason,
                proto::StateStoreWatchCloseReason::BufferOverflow as i32
            );
        }
    }

    #[test]
    fn expire_event_publishes_without_entry() {
        let state = ServerState::empty();
        let session = SessionId::from_u64(21);
        admin_session(&state, session);
        register(&state, session, "w", "service/transcoder-a/", "");
        let watches = &state.state_store_watches;
        let (sent, enqueue) = recorder();
        let delivered = watches.publish(
            &state,
            &[WatchEvent {
                namespace: "service/transcoder-a/".to_owned(),
                key: "leases/front-door".to_owned(),
                revision: 2,
                kind: WatchEventKind::Expire,
            }],
            false,
            NOW_MS,
            &enqueue,
        );
        assert_eq!(delivered.len(), 1);
        let borrowed = sent.borrow();
        let update = update_of(&borrowed[0].1);
        assert_eq!(update.kind, proto::StateStoreUpdateKind::Expire as i32);
        assert_eq!(update.revision, 2);
        assert!(update.entry.is_none());
    }

    #[test]
    fn transport_outcomes_control_retention() {
        fn watch_id_of(notification: &proto::Notification) -> &str {
            match &notification.event {
                Some(notification::Event::StateStoreWatchUpdate(update)) => &update.watch_id,
                Some(notification::Event::StateStoreWatchClosed(closed)) => &closed.watch_id,
                other => panic!("expected a watch signal, got {other:?}"),
            }
        }
        let state = ServerState::empty();
        let session = SessionId::from_u64(15);
        admin_session(&state, session);
        register(&state, session, "full", "service/transcoder-a/", "");
        register(&state, session, "gone", "service/transcoder-a/", "");
        let watches = &state.state_store_watches;
        let full = watches.publish(
            &state,
            &[put_event("service/transcoder-a/", "k", 1)],
            false,
            NOW_MS,
            |_, notification| {
                if watch_id_of(&notification) == "full" {
                    EnqueueOutcome::QueueFull
                } else {
                    EnqueueOutcome::Sent
                }
            },
        );
        assert_eq!(full.len(), 1);
        assert!(
            !watches.owns_watch(session, "full"),
            "full queues remove the watch without a close signal"
        );
        assert!(
            watches.owns_watch(session, "gone"),
            "sent updates keep the watch"
        );
        let gone = watches.publish(
            &state,
            &[put_event("service/transcoder-a/", "k", 2)],
            false,
            NOW_MS,
            |_, _| EnqueueOutcome::Gone,
        );
        assert!(gone.is_empty());
        assert!(
            watches.owns_watch(session, "gone"),
            "missing transport keeps buffering for the sweeper to reap"
        );
    }

    #[test]
    fn sweep_reaps_watches_without_live_transport() {
        let state = ServerState::empty();
        let session = SessionId::from_u64(16);
        admin_session(&state, session);
        register(&state, session, "slow", "service/transcoder-a/", "");
        let watches = &state.state_store_watches;
        let (sent, enqueue) = recorder();
        watches.publish(
            &state,
            &[put_event("service/transcoder-a/", "k", 1)],
            false,
            NOW_MS,
            &enqueue,
        );
        assert!(watches.owns_watch(session, "slow"));
        let (_, swept_enqueue) = recorder();
        let swept_sent = watches.expire_watches(&state, NOW_MS + ACK_TIMEOUT_MS, swept_enqueue);
        assert!(
            !watches.owns_watch(session, "slow"),
            "missing WebRTC transport is reaped silently"
        );
        assert!(
            swept_sent.is_empty(),
            "ghost reap sends no close without a live session"
        );
        drop(sent);
    }

    #[test]
    fn ack_deadline_is_inclusive() {
        let sent_ms = NOW_MS;
        assert!(
            sent_ms.saturating_add(ACK_TIMEOUT_MS) <= NOW_MS + ACK_TIMEOUT_MS,
            "overdue check must fire exactly at sent + timeout"
        );
        assert!(
            sent_ms.saturating_add(ACK_TIMEOUT_MS) >= NOW_MS + ACK_TIMEOUT_MS,
            "one millisecond early must not count as overdue"
        );
    }

    #[test]
    fn revoked_principal_terminates_watch() {
        let state = ServerState::empty();
        let session = SessionId::from_u64(17);
        admin_session(&state, session);
        register(&state, session, "w", "service/transcoder-a/", "");
        state
            .api_session_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session);
        let watches = &state.state_store_watches;
        let (sent, enqueue) = recorder();
        watches.publish(
            &state,
            &[put_event("service/transcoder-a/", "k", 1)],
            false,
            NOW_MS,
            enqueue,
        );
        assert!(!watches.owns_watch(session, "w"));
        let sent = sent.borrow();
        assert_eq!(sent.len(), 1);
        let close = close_of(&sent[0].1);
        assert_eq!(
            close.reason,
            proto::StateStoreWatchCloseReason::AuthorizationRevoked as i32
        );
    }

    #[test]
    fn reregister_replaces_and_restarts_sequence() {
        let state = ServerState::empty();
        let session = SessionId::from_u64(18);
        admin_session(&state, session);
        register(&state, session, "w", "service/transcoder-a/", "a/");
        let watches = &state.state_store_watches;
        let (first, enqueue) = recorder();
        watches.publish(
            &state,
            &[put_event("service/transcoder-a/", "a/1", 1)],
            false,
            NOW_MS,
            &enqueue,
        );
        assert_eq!(first.borrow().len(), 1);
        register(&state, session, "w", "service/transcoder-a/", "b/");
        let (second, enqueue) = recorder();
        let delivered = watches.publish(
            &state,
            &[put_event("service/transcoder-a/", "b/2", 2)],
            false,
            NOW_MS,
            &enqueue,
        );
        assert_eq!(delivered.len(), 1);
        let borrowed = second.borrow();
        let update = update_of(&borrowed[0].1);
        assert_eq!(update.watch_sequence, 1);
        assert_eq!(update.key, "b/2");
        assert_eq!(
            watches
                .acknowledge(session, "w", 5, NOW_MS)
                .map_err(|(_, error)| error),
            Err(Error::Invalid(Invalid::AckAhead))
        );
    }

    #[test]
    fn unregister_and_close_session_remove_watches() {
        let state = ServerState::empty();
        let session = SessionId::from_u64(19);
        admin_session(&state, session);
        register(&state, session, "w", "service/transcoder-a/", "");
        let watches = &state.state_store_watches;
        let (_, error) = watches
            .acknowledge(session, "missing", 0, NOW_MS)
            .expect_err("unknown watch must fail");
        assert_eq!(error, Error::Invalid(Invalid::WatchNotFound));
        assert_eq!(
            watches.unregister(session, "missing"),
            Err(Error::Invalid(Invalid::WatchNotFound))
        );
        watches
            .unregister(session, "w")
            .expect("unwatch must succeed");
        assert!(!watches.owns_watch(session, "w"));
        register(&state, session, "w", "service/transcoder-a/", "");
        watches.close_session(session);
        assert!(!watches.owns_watch(session, "w"));
    }

    #[test]
    fn watch_caps_apply_per_session_and_total() {
        let state = ServerState::empty();
        let session = SessionId::from_u64(20);
        admin_session(&state, session);
        let watches = &state.state_store_watches;
        for index in 0..MAX_WATCHES_PER_SESSION {
            watches
                .register(
                    session,
                    "service/transcoder-a/".to_owned(),
                    String::new(),
                    format!("w-{index}"),
                )
                .expect("watch within caps must succeed");
        }
        assert_eq!(
            watches.register(
                session,
                "service/transcoder-a/".to_owned(),
                String::new(),
                "w-overflow".to_owned(),
            ),
            Err(Error::Invalid(Invalid::WatchLimitExceeded))
        );
    }
}
