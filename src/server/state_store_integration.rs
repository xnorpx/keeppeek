//! Client/server integration coverage for the state-store watch path.
//!
//! These tests drive real [`super::state_store::dispatch`] calls against a
//! real [`super::ServerState`], publish through the production watch fan-out
//! into staged WebRTC sessions, pump the session queues through the real
//! control-notification flush (the exact [`ControlEnvelope`] bytes the data
//! channel would carry), and assert on decoded client-side state. The only
//! skipped layer is socket encryption and delivery.

#[cfg(test)]
mod tests {
    use super::super::state_store_watch::ACK_TIMEOUT_MS;
    use super::super::{ApiPrincipal, ServerState};
    use super::super::{state_store, state_store_watch};
    use crate::api::proto::{
        self, control_envelope, ok as control_ok, state_store_command, state_store_result,
    };
    use crate::webrtc::SessionId;
    use prost::Message;
    use prost_types::{Struct, Value, value::Kind};
    use std::collections::BTreeMap;
    use std::net::{IpAddr, Ipv4Addr};

    const NAMESPACE: &str = "service/transcoder-a/";
    const SCHEMA: &str = "keeppeek.media-intent.v1";

    fn now_ms() -> u64 {
        u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        )
        .unwrap_or(u64::MAX)
    }

    fn admin_session(state: &ServerState, session: SessionId) {
        use crate::access::{ClientClassification, ClientClassificationReason};
        use std::time::Instant;
        state
            .api_session_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                session,
                super::super::ApiSessionRecord {
                    principal: ApiPrincipal::local(IpAddr::V4(Ipv4Addr::LOCALHOST)),
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
                },
            );
    }

    fn local_principal() -> ApiPrincipal {
        ApiPrincipal::local(IpAddr::V4(Ipv4Addr::LOCALHOST))
    }

    fn media_intent_value(role: &str) -> Struct {
        let mut fields = BTreeMap::from([
            (
                "role".to_owned(),
                Value {
                    kind: Some(Kind::StringValue(role.to_owned())),
                },
            ),
            (
                "source_id".to_owned(),
                Value {
                    kind: Some(Kind::StringValue("front-door".to_owned())),
                },
            ),
            (
                "media_kind".to_owned(),
                Value {
                    kind: Some(Kind::StringValue("video".to_owned())),
                },
            ),
        ]);
        fields.insert(
            "desired".to_owned(),
            Value {
                kind: Some(Kind::BoolValue(true)),
            },
        );
        if role == "publish" {
            fields.insert(
                "recording_mode".to_owned(),
                Value {
                    kind: Some(Kind::StringValue("disabled".to_owned())),
                },
            );
        }
        Struct { fields }
    }

    fn put_command(key: &str, expected_revision: Option<u64>) -> proto::StateStoreCommand {
        proto::StateStoreCommand {
            action: Some(state_store_command::Action::Put(proto::PutState {
                namespace: NAMESPACE.to_owned(),
                key: key.to_owned(),
                schema: SCHEMA.to_owned(),
                value: Some(media_intent_value("publish")),
                expected_revision,
                ttl: None,
            })),
        }
    }

    fn watch_command(watch_id: &str, key_prefix: &str) -> proto::StateStoreCommand {
        proto::StateStoreCommand {
            action: Some(state_store_command::Action::Watch(proto::WatchState {
                namespace: NAMESPACE.to_owned(),
                key_prefix: key_prefix.to_owned(),
                watch_id: watch_id.to_owned(),
            })),
        }
    }

    fn ack_command(watch_id: &str, applied_sequence: u64) -> proto::StateStoreCommand {
        proto::StateStoreCommand {
            action: Some(state_store_command::Action::WatchAck(
                proto::WatchStateAck {
                    watch_id: watch_id.to_owned(),
                    applied_sequence,
                },
            )),
        }
    }

    fn unwatch_command(watch_id: &str) -> proto::StateStoreCommand {
        proto::StateStoreCommand {
            action: Some(state_store_command::Action::Unwatch(proto::UnwatchState {
                watch_id: watch_id.to_owned(),
            })),
        }
    }

    fn dispatch_put(state: &ServerState, session: SessionId, key: &str, expected: Option<u64>) {
        state_store::dispatch(
            state,
            session,
            &local_principal(),
            put_command(key, expected),
        )
        .expect("integration put must succeed");
    }

    fn dispatch_watch(
        state: &ServerState,
        session: SessionId,
        watch_id: &str,
        key_prefix: &str,
    ) -> proto::StateWatchSnapshot {
        let control_ok::Result::StateStoreResult(result) = state_store::dispatch(
            state,
            session,
            &local_principal(),
            watch_command(watch_id, key_prefix),
        )
        .expect("integration watch must succeed") else {
            panic!("integration watch must return a StateStoreResult");
        };
        let Some(state_store_result::Result::Watch(snapshot)) = result.result else {
            panic!("integration watch must return a snapshot");
        };
        snapshot
    }

    fn dispatch_ack(
        state: &ServerState,
        session: SessionId,
        watch_id: &str,
        applied_sequence: u64,
    ) -> u64 {
        let control_ok::Result::StateStoreResult(result) = state_store::dispatch(
            state,
            session,
            &local_principal(),
            ack_command(watch_id, applied_sequence),
        )
        .expect("integration ack must succeed") else {
            panic!("integration ack must return a StateStoreResult");
        };
        let Some(state_store_result::Result::WatchAck(ack)) = result.result else {
            panic!("integration ack must return an acknowledgement");
        };
        ack.applied_sequence
    }

    struct Client<Pump: FnMut() -> Vec<Vec<u8>>> {
        session: SessionId,
        pump: Pump,
    }

    fn stage(state: &ServerState, id: u64) -> Client<impl FnMut() -> Vec<Vec<u8>>> {
        let session = SessionId::from_u64(id);
        admin_session(state, session);
        let pump = state.webrtc.stage_api_session_pump(session);
        Client { session, pump }
    }

    impl<Pump: FnMut() -> Vec<Vec<u8>>> Client<Pump> {
        fn pump_into(&mut self, mirror: &mut TestClient) {
            for payload in (self.pump)() {
                mirror.receive(&payload);
            }
        }

        fn pending_payloads(&mut self) -> Vec<Vec<u8>> {
            (self.pump)()
        }
    }

    struct TestClient {
        watch_id: String,
        snapshot_revision: Option<u64>,
        entries: BTreeMap<String, proto::StateEntry>,
        last_sequence: u64,
        last_revision: u64,
        closes: Vec<i32>,
        diverged: bool,
    }

    impl TestClient {
        fn new(watch_id: &str) -> Self {
            Self {
                watch_id: watch_id.to_owned(),
                snapshot_revision: None,
                entries: BTreeMap::new(),
                last_sequence: 0,
                last_revision: 0,
                closes: Vec::new(),
                diverged: false,
            }
        }

        fn install_snapshot(&mut self, snapshot: &proto::StateWatchSnapshot) {
            assert_eq!(snapshot.watch_id, self.watch_id);
            self.snapshot_revision = Some(snapshot.snapshot_revision);
            self.entries = snapshot
                .entries
                .iter()
                .map(|entry| (entry.key.clone(), entry.clone()))
                .collect();
            self.last_sequence = 0;
            self.last_revision = snapshot.snapshot_revision;
            self.diverged = false;
        }

        fn receive(&mut self, payload: &[u8]) {
            use crate::api::proto::notification::Event;
            let envelope = crate::api::proto::ControlEnvelope::decode(payload)
                .expect("wire payload must decode as a control envelope");
            let Some(control_envelope::Message::Notification(notification)) = envelope.message
            else {
                panic!("wire payload must carry a notification");
            };
            match notification.event {
                Some(Event::StateStoreWatchUpdate(update)) => self.apply_update(&update),
                Some(Event::StateStoreWatchClosed(closed)) => {
                    assert_eq!(closed.watch_id, self.watch_id);
                    self.closes.push(closed.reason);
                }
                other => panic!("unexpected wire event: {other:?}"),
            }
        }

        fn apply_update(&mut self, update: &proto::StateStoreWatchUpdate) {
            assert_eq!(update.watch_id, self.watch_id);
            let Some(snapshot_revision) = self.snapshot_revision else {
                panic!("client must hold a snapshot before applying updates");
            };
            assert!(
                update.revision > snapshot_revision,
                "every update must follow the installed snapshot",
            );
            if update.watch_sequence != self.last_sequence + 1 {
                self.diverged = true;
                return;
            }
            assert!(
                update.revision > self.last_revision,
                "delivered revisions must advance in commit order",
            );
            let kind = proto::StateStoreUpdateKind::try_from(update.kind)
                .expect("update kind must be a known variant");
            match kind {
                proto::StateStoreUpdateKind::Put => {
                    let entry = update.entry.clone().expect("put must carry an entry");
                    self.entries.insert(update.key.clone(), entry);
                }
                proto::StateStoreUpdateKind::Delete | proto::StateStoreUpdateKind::Expire => {
                    self.entries.remove(&update.key);
                }
                proto::StateStoreUpdateKind::Unspecified => {
                    panic!("server must never publish an unspecified update kind");
                }
            }
            self.last_sequence = update.watch_sequence;
            self.last_revision = update.revision;
        }
    }

    fn assert_converged(left: &TestClient, right: &TestClient) {
        assert_eq!(left.snapshot_revision, right.snapshot_revision);
        assert_eq!(left.entries, right.entries);
        assert!(!left.diverged && !right.diverged);
    }

    #[test]
    fn snapshot_before_updates_two_clients_converge() {
        let state = ServerState::empty();
        let mut first = stage(&state, 71_001);
        let mut second = stage(&state, 71_002);
        dispatch_put(&state, first.session, "intents/front-door", None);
        dispatch_put(&state, first.session, "intents/back-door", None);
        let mut mirror_a = TestClient::new("w");
        let mut mirror_b = TestClient::new("w");
        mirror_a.install_snapshot(&dispatch_watch(&state, first.session, "w", ""));
        mirror_b.install_snapshot(&dispatch_watch(&state, second.session, "w", ""));
        assert_eq!(mirror_a.snapshot_revision, Some(2));
        dispatch_put(&state, first.session, "intents/side-door", None);
        first.pump_into(&mut mirror_a);
        second.pump_into(&mut mirror_b);
        assert_eq!(mirror_a.last_sequence, 1);
        assert_eq!(mirror_a.last_revision, 3);
        assert_converged(&mirror_a, &mirror_b);
        assert_eq!(mirror_a.entries.len(), 3);
        state.webrtc.remove_api_session(first.session);
        state.webrtc.remove_api_session(second.session);
    }

    #[test]
    fn concurrent_mutation_and_registration_converge() {
        use std::sync::Arc;
        let state = Arc::new(ServerState::empty());
        let mut first = stage(&state, 71_011);
        let mut second = stage(&state, 71_012);
        let mut mirror_a = TestClient::new("w");
        let mut mirror_b = TestClient::new("w");
        mirror_a.install_snapshot(&dispatch_watch(&state, first.session, "w", ""));
        mirror_b.install_snapshot(&dispatch_watch(&state, second.session, "w", ""));
        let mut handles = Vec::new();
        for worker in 0..2 {
            let state = Arc::clone(&state);
            handles.push(std::thread::spawn(move || {
                for index in 0..10 {
                    let key = format!("intents/cam-{worker}-{index}");
                    dispatch_put(&state, SessionId::from_u64(71_011), &key, None);
                }
            }));
        }
        for handle in handles {
            handle.join().expect("worker must finish");
        }
        first.pump_into(&mut mirror_a);
        second.pump_into(&mut mirror_b);
        assert_eq!(mirror_a.last_sequence, 20);
        assert_eq!(mirror_a.last_revision, 20);
        assert_converged(&mirror_a, &mirror_b);
        assert_eq!(mirror_a.entries.len(), 20);
        assert_eq!(dispatch_ack(&state, first.session, "w", 20), 20);
        state.webrtc.remove_api_session(first.session);
        state.webrtc.remove_api_session(second.session);
    }

    #[test]
    fn prefix_watch_ignores_out_of_scope_revisions() {
        let state = ServerState::empty();
        let mut client = stage(&state, 71_021);
        let mut mirror = TestClient::new("w");
        mirror.install_snapshot(&dispatch_watch(&state, client.session, "w", "intents/"));
        dispatch_put(&state, client.session, "intents/front-door", None);
        dispatch_put(&state, client.session, "leases/front-door", None);
        client.pump_into(&mut mirror);
        assert_eq!(mirror.last_sequence, 1);
        assert_eq!(mirror.entries.len(), 1);
        assert!(mirror.entries.contains_key("intents/front-door"));
        assert!(!mirror.diverged);
        state.webrtc.remove_api_session(client.session);
    }

    #[test]
    fn gap_in_sequences_forces_resync() {
        let state = ServerState::empty();
        let mut client = stage(&state, 71_031);
        let mut mirror = TestClient::new("w");
        mirror.install_snapshot(&dispatch_watch(&state, client.session, "w", ""));
        dispatch_put(&state, client.session, "intents/front-door", None);
        dispatch_put(&state, client.session, "intents/back-door", None);
        dispatch_put(&state, client.session, "intents/side-door", None);
        let mut payloads = client.pending_payloads();
        assert_eq!(payloads.len(), 3);
        mirror.receive(&payloads.remove(0));
        assert!(!mirror.diverged);
        mirror.receive(&payloads.remove(1));
        assert!(mirror.diverged);
        let mut fresh = TestClient::new("w2");
        fresh.install_snapshot(&dispatch_watch(&state, client.session, "w2", ""));
        assert_eq!(fresh.snapshot_revision, Some(3));
        assert_eq!(fresh.entries.len(), 3);
        assert!(!fresh.diverged);
        state.webrtc.remove_api_session(client.session);
    }

    #[test]
    fn unwatch_and_disconnect_stop_delivery() {
        let state = ServerState::empty();
        let mut client = stage(&state, 71_041);
        let mut mirror = TestClient::new("w");
        mirror.install_snapshot(&dispatch_watch(&state, client.session, "w", ""));
        state_store::dispatch(
            &state,
            client.session,
            &local_principal(),
            unwatch_command("w"),
        )
        .expect("integration unwatch must succeed");
        assert!(!state.state_store_watches.owns_watch(client.session, "w"));
        dispatch_put(&state, client.session, "intents/front-door", None);
        assert!(client.pending_payloads().is_empty());
        let second = stage(&state, 71_042);
        let snapshot = dispatch_watch(&state, second.session, "w", "");
        assert_eq!(snapshot.snapshot_revision, 1);
        state.state_store_watches.close_session(second.session);
        assert!(!state.state_store_watches.owns_watch(second.session, "w"));
        state.webrtc.remove_api_session(client.session);
        state.webrtc.remove_api_session(second.session);
    }

    #[test]
    fn stalled_subscriber_times_out_with_ack_timeout_close() {
        let state = ServerState::empty();
        let mut client = stage(&state, 71_051);
        let mut mirror = TestClient::new("w");
        mirror.install_snapshot(&dispatch_watch(&state, client.session, "w", ""));
        dispatch_put(&state, client.session, "intents/front-door", None);
        client.pump_into(&mut mirror);
        assert_eq!(mirror.last_sequence, 1);
        let sent = state.state_store_watches.expire_watches(
            &state,
            now_ms()
                .saturating_add(ACK_TIMEOUT_MS)
                .saturating_add(120_000),
            |session_id, notification| {
                state_store_watch::enqueue_notification(&state, session_id, notification)
            },
        );
        assert_eq!(sent.len(), 1);
        client.pump_into(&mut mirror);
        assert_eq!(
            mirror.closes,
            vec![proto::StateStoreWatchCloseReason::AckTimeout as i32]
        );
        assert!(!state.state_store_watches.owns_watch(client.session, "w"));
        state.webrtc.remove_api_session(client.session);
    }

    #[test]
    fn advancing_ack_survives_early_expiry_then_times_out() {
        let state = ServerState::empty();
        let mut client = stage(&state, 71_061);
        let mut mirror = TestClient::new("w");
        mirror.install_snapshot(&dispatch_watch(&state, client.session, "w", ""));
        dispatch_put(&state, client.session, "intents/front-door", None);
        let early = state.state_store_watches.expire_watches(
            &state,
            now_ms().saturating_add(1_000),
            |session_id, notification| {
                state_store_watch::enqueue_notification(&state, session_id, notification)
            },
        );
        assert!(early.is_empty());
        assert!(state.state_store_watches.owns_watch(client.session, "w"));
        assert_eq!(dispatch_ack(&state, client.session, "w", 1), 1);
        dispatch_put(&state, client.session, "intents/back-door", None);
        client.pump_into(&mut mirror);
        assert_eq!(mirror.last_sequence, 2);
        let late = state.state_store_watches.expire_watches(
            &state,
            now_ms()
                .saturating_add(ACK_TIMEOUT_MS)
                .saturating_add(120_000),
            |session_id, notification| {
                state_store_watch::enqueue_notification(&state, session_id, notification)
            },
        );
        assert_eq!(late.len(), 1);
        client.pump_into(&mut mirror);
        assert_eq!(
            mirror.closes,
            vec![proto::StateStoreWatchCloseReason::AckTimeout as i32]
        );
        state.webrtc.remove_api_session(client.session);
    }

    #[test]
    fn queue_full_terminates_watch_and_session() {
        let state = ServerState::empty();
        let client = stage(&state, 71_071);
        let mut mirror = TestClient::new("w");
        mirror.install_snapshot(&dispatch_watch(&state, client.session, "w", ""));
        loop {
            match state
                .webrtc
                .try_enqueue_api_notification(client.session, proto::Notification::default())
            {
                Ok(true) => {}
                Ok(false) => break,
                Err(error) => panic!("staging fill must not lose the session: {error:?}"),
            }
        }
        dispatch_put(&state, client.session, "intents/front-door", None);
        assert!(!state.state_store_watches.owns_watch(client.session, "w"));
        assert!(
            state
                .webrtc
                .try_enqueue_api_notification(client.session, proto::Notification::default())
                .is_err(),
            "the saturated session must be gone after termination",
        );
    }

    #[test]
    fn reconnect_gets_fresh_snapshot_and_valid_cas() {
        let state = ServerState::empty();
        let first = stage(&state, 71_081);
        dispatch_put(&state, first.session, "intents/front-door", None);
        dispatch_put(&state, first.session, "intents/back-door", None);
        let mut mirror = TestClient::new("w");
        mirror.install_snapshot(&dispatch_watch(&state, first.session, "w", ""));
        assert_eq!(mirror.snapshot_revision, Some(2));
        state.state_store_watches.close_session(first.session);
        state.webrtc.remove_api_session(first.session);
        let second = stage(&state, 71_082);
        let mut reconnected = TestClient::new("w");
        let snapshot = dispatch_watch(&state, second.session, "w", "");
        reconnected.install_snapshot(&snapshot);
        assert_eq!(reconnected.snapshot_revision, Some(2));
        assert_eq!(reconnected.entries.len(), 2);
        let entry_revision = reconnected.entries["intents/front-door"].revision;
        dispatch_put(
            &state,
            second.session,
            "intents/front-door",
            Some(entry_revision),
        );
        state_store::dispatch(
            &state,
            second.session,
            &local_principal(),
            put_command("intents/front-door", Some(entry_revision)),
        )
        .expect_err("a stale CAS against the restored revision must conflict");
        state.webrtc.remove_api_session(second.session);
    }
}
