use reo_proto::{
    alarm::{AlarmCommand, AlarmEventData},
    session::{BcSession, Command, Input},
};
use std::{
    collections::{HashMap, VecDeque},
    sync::mpsc::{SyncSender, TrySendError},
};

use crate::{
    keeppeek::KeepPeekEvent,
    storage::metadata::{EventSource, TimelineEvent, event_icon},
};

use super::{
    ReolinkLoop, alarm_event_kinds, end_active_motion_events, random_event_id, unix_time_ms,
};

const PENDING_ENDINGS_MAX: usize = 128;
const ENDINGS_PER_TICK: usize = 4;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct Policy {
    pub enabled: bool,
    pub revision: u64,
    pub record_motion: bool,
}

#[derive(Debug, Default)]
pub(super) struct Gate {
    logged_in: bool,
    subscribed: bool,
    policy: Policy,
    pending_close_ms: Option<i64>,
}

impl Gate {
    pub(super) const fn logged_in(&mut self) {
        self.logged_in = true;
    }

    const fn update(&mut self, policy: Policy) {
        self.policy = policy;
    }

    pub(super) fn reconcile(
        &mut self,
        policy: Policy,
        now_ms: i64,
        active: &mut HashMap<String, String>,
        sent: &SyncSender<KeepPeekEvent>,
    ) -> bool {
        let changed = self.policy != policy;
        if self.policy.enabled && changed && !active.is_empty() {
            self.pending_close_ms.get_or_insert(now_ms);
        }
        self.update(policy);
        if let Some(end_time_ms) = self.pending_close_ms
            && try_end_events(active, end_time_ms, sent)
        {
            self.pending_close_ms = None;
        }
        changed
    }

    pub(super) fn permits(&self, policy: Policy) -> bool {
        self.logged_in
            && self.subscribed
            && policy.enabled
            && self.policy == policy
            && self.pending_close_ms.is_none()
    }

    pub(super) fn subscribe(
        &mut self,
        channel: u8,
        send: impl FnOnce(Command) -> Result<(), reo_proto::BcError>,
    ) -> Result<(), reo_proto::BcError> {
        if self.logged_in
            && self.policy.enabled
            && !self.subscribed
            && self.pending_close_ms.is_none()
        {
            send(Command::Alarm(AlarmCommand::StartMotionAlarm { channel }))?;
            self.subscribed = true;
        }
        Ok(())
    }
}

pub(super) fn try_end_events(
    active: &mut HashMap<String, String>,
    end_time_ms: i64,
    sent: &SyncSender<KeepPeekEvent>,
) -> bool {
    assert!(
        active.len() <= PENDING_ENDINGS_MAX,
        "too many pending Reolink endings"
    );
    for _ in 0..ENDINGS_PER_TICK {
        let Some((kind, id)) = active.iter().next() else {
            break;
        };
        let kind = kind.clone();
        match sent.try_send(KeepPeekEvent::TimelineEventEnded {
            id: id.clone(),
            end_time_ms,
        }) {
            Ok(()) => {
                active.remove(&kind);
            }
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => break,
        }
    }
    active.is_empty()
}

pub(super) fn invalidate_snapshots(pending: &mut VecDeque<Vec<String>>, in_flight: bool) {
    pending.clear();
    if in_flight {
        pending.push_back(Vec::new());
    }
}

impl ReolinkLoop {
    pub(super) fn event_policy(&self) -> Policy {
        let revision_before = self.health.events.vendor_revision(self.camera_ip);
        let enabled = self.health.events.vendor_enabled(self.camera_ip);
        let record_motion = self
            .health
            .events
            .record_motion(self.camera_ip, self.record_generic_motion_events);
        let revision = self.health.events.vendor_revision(self.camera_ip);
        Policy {
            enabled: enabled && revision_before == revision,
            revision,
            record_motion,
        }
    }

    pub(super) fn poll_event_policy(
        &self,
        gate: &mut Gate,
        active: &mut HashMap<String, String>,
        session: &mut BcSession,
    ) -> bool {
        let changed = gate.reconcile(self.event_policy(), unix_time_ms(), active, &self.tx);
        if let Err(error) = gate.subscribe(self.channel, |command| {
            session.handle_input(Input::Command(command))
        }) {
            tracing::warn!(ip = %self.camera_ip, %error, "camera motion events are unavailable");
        }
        changed
    }

    pub(super) fn record_alarm_events(
        &self,
        events: &[AlarmEventData],
        record_motion: bool,
        active: &mut HashMap<String, String>,
    ) -> Vec<String> {
        let mut started_ids = Vec::with_capacity(4);
        let mut received_alarm = false;
        let mut received_active_alarm = false;
        for data in events {
            if data.channel != self.channel {
                continue;
            }
            received_alarm = true;
            for kind in alarm_event_kinds(data, record_motion) {
                received_active_alarm = true;
                if active.contains_key(&kind) {
                    continue;
                }
                started_ids.push(self.start_alarm_event(kind, active));
            }
        }
        if received_alarm && !received_active_alarm {
            end_active_motion_events(&self.tx, active, unix_time_ms());
        }
        started_ids
    }

    fn start_alarm_event(&self, kind: String, active: &mut HashMap<String, String>) -> String {
        let event_id = random_event_id();
        active.insert(kind.clone(), event_id.clone());
        let icon_key = event_icon(None, &kind).key.to_owned();
        let event = TimelineEvent {
            id: event_id.clone(),
            revision: 1,
            camera_id: self.camera_ip.to_string(),
            stream: None,
            source: EventSource::Camera,
            kind,
            start_time_ms: unix_time_ms(),
            end_time_ms: None,
            confidence: None,
            bbox: None,
            bbox_attachment_id: None,
            zone: None,
            text: None,
            payload: None,
            attachments: Vec::new(),
            canonical_attachment_id: None,
            icon_key,
            rejected_icon_key: None,
            thumbnail_filename: None,
        };
        let _ = self.tx.send(KeepPeekEvent::TimelineEventStarted {
            event: Box::new(event),
        });
        event_id
    }
}

#[cfg(test)]
mod vendor_event_policy {
    use super::{Gate, Policy, ReolinkLoop, invalidate_snapshots, try_end_events};
    use crate::{
        cameras::{
            CameraTransport,
            events::{EventConfig, EventMode},
        },
        keeppeek::KeepPeekEvent,
        shutdown::Shutdown,
        stats::HealthRegistry,
    };
    use reo_proto::{
        alarm::AlarmCommand,
        session::{BcSession, BcSessionConfig, Command, Input, Output},
    };
    use std::{
        collections::{HashMap, VecDeque},
        sync::mpsc,
        time::{Duration, Instant},
    };

    fn camera_loop(sent: mpsc::SyncSender<KeepPeekEvent>) -> ReolinkLoop {
        ReolinkLoop {
            camera_ip: "192.0.2.1".parse().unwrap(),
            camera_name: None,
            camera_brand: None,
            camera_uid: None,
            username: String::new(),
            password: String::new(),
            transport: CameraTransport::Tcp,
            channel: 2,
            enable_main: true,
            enable_sub: true,
            main_expected_width: 3840,
            main_expected_height: 2160,
            main_expected_fps: 25.0,
            sub_expected_width: 640,
            sub_expected_height: 360,
            sub_expected_fps: 15.0,
            record_generic_motion_events: false,
            storage: None,
            live: None,
            health: HealthRegistry::default(),
            tx: sent,
            shutdown: Shutdown::new(),
            battery_wake: None,
        }
    }

    fn configure_events(camera: &ReolinkLoop, mode: EventMode) {
        camera.health.events.configure(
            camera.camera_ip,
            EventConfig {
                mode,
                ..EventConfig::default()
            },
            false,
            camera.shutdown.clone(),
        );
    }

    #[test]
    fn vendor_toggle_subscribes_only_after_login_and_hot_enable() {
        let mut gate = Gate::default();
        let (queued, commands) = mpsc::sync_channel(4);
        let send = |command| {
            queued.try_send(command).unwrap();
            Ok::<(), reo_proto::BcError>(())
        };

        gate.update(Policy {
            enabled: false,
            revision: 1,
            ..Policy::default()
        });
        gate.subscribe(2, send).unwrap();
        assert!(matches!(
            commands.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        gate.logged_in();
        gate.subscribe(2, send).unwrap();
        assert!(matches!(
            commands.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        gate.update(Policy {
            enabled: true,
            revision: 2,
            ..Policy::default()
        });
        gate.subscribe(2, send).unwrap();
        gate.subscribe(2, send).unwrap();

        assert!(matches!(
            commands.try_iter().collect::<Vec<_>>().as_slice(),
            [Command::Alarm(AlarmCommand::StartMotionAlarm {
                channel: 2
            })]
        ));
    }

    #[test]
    fn vendor_toggle_enabled_policy_waits_for_login() {
        let mut gate = Gate::default();
        gate.update(Policy {
            enabled: true,
            revision: 1,
            ..Policy::default()
        });
        gate.subscribe(0, |_| panic!("must not subscribe before login"))
            .unwrap();

        gate.logged_in();
        let mut commands = Vec::new();
        gate.subscribe(0, |command| {
            commands.push(command);
            Ok(())
        })
        .unwrap();

        assert!(matches!(
            commands.as_slice(),
            [Command::Alarm(AlarmCommand::StartMotionAlarm {
                channel: 0
            })]
        ));
    }

    #[test]
    fn disable_retains_every_ending_until_capacity_returns_exactly_once() {
        let (sent, received) = mpsc::sync_channel(1);
        sent.try_send(KeepPeekEvent::TimelineEventEnded {
            id: "occupied".to_owned(),
            end_time_ms: 0,
        })
        .unwrap();
        let mut active = HashMap::from([
            ("motion".to_owned(), "old-motion".to_owned()),
            ("person".to_owned(), "old-person".to_owned()),
            ("animal".to_owned(), "old-animal".to_owned()),
            ("vehicle".to_owned(), "old-vehicle".to_owned()),
        ]);
        let original = active.clone();

        for _ in 0..3 {
            assert!(!try_end_events(&mut active, 123, &sent));
            assert_eq!(active, original);
        }
        assert!(matches!(
            received.try_recv().unwrap(),
            KeepPeekEvent::TimelineEventEnded { id, .. } if id == "occupied"
        ));

        let mut delivered = Vec::new();
        for remaining in (0..4).rev() {
            assert_eq!(try_end_events(&mut active, 123, &sent), remaining == 0);
            assert_eq!(active.len(), remaining);
            match received.try_recv().unwrap() {
                KeepPeekEvent::TimelineEventEnded { id, end_time_ms } => {
                    assert_eq!(end_time_ms, 123);
                    delivered.push(id);
                }
                _ => panic!("expected an ending"),
            }
        }
        delivered.sort();
        assert_eq!(
            delivered,
            ["old-animal", "old-motion", "old-person", "old-vehicle"]
        );
        assert!(try_end_events(&mut active, 456, &sent));
        assert!(matches!(
            received.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn disable_retains_ids_when_the_receiver_disconnects() {
        let (sent, received) = mpsc::sync_channel(1);
        drop(received);
        let mut active = HashMap::from([("person".to_owned(), "old-person".to_owned())]);

        assert!(!try_end_events(&mut active, 123, &sent));
        assert_eq!(active.get("person").map(String::as_str), Some("old-person"));
    }

    #[test]
    fn vendor_toggle_reenable_waits_for_older_endings_without_resubscribing() {
        let (sent, received) = mpsc::sync_channel(1);
        sent.try_send(KeepPeekEvent::TimelineEventEnded {
            id: "occupied".to_owned(),
            end_time_ms: 0,
        })
        .unwrap();
        let mut active = HashMap::from([("person".to_owned(), "old-person".to_owned())]);
        let mut gate = Gate::default();
        let enabled = Policy {
            enabled: true,
            revision: 1,
            ..Policy::default()
        };
        gate.update(enabled);
        gate.logged_in();
        gate.subscribe(0, |_| Ok(())).unwrap();
        assert!(gate.permits(enabled));

        gate.reconcile(
            Policy {
                enabled: false,
                revision: 2,
                ..Policy::default()
            },
            200,
            &mut active,
            &sent,
        );
        assert!(!gate.permits(enabled));
        let reenabled = Policy {
            enabled: true,
            revision: 3,
            ..Policy::default()
        };
        for now_ms in [300, 400, 500] {
            gate.reconcile(reenabled, now_ms, &mut active, &sent);
            assert!(!gate.permits(reenabled));
            assert_eq!(active.get("person").map(String::as_str), Some("old-person"));
            gate.subscribe(0, |_| panic!("must retain the existing subscription"))
                .unwrap();
        }
        received.try_recv().unwrap();

        gate.reconcile(reenabled, 600, &mut active, &sent);

        assert!(active.is_empty());
        assert!(gate.permits(reenabled));
        assert!(matches!(
            received.try_recv().unwrap(),
            KeepPeekEvent::TimelineEventEnded { id, end_time_ms: 200 } if id == "old-person"
        ));
        gate.reconcile(reenabled, 700, &mut active, &sent);
        gate.subscribe(0, |_| panic!("must not resubscribe after recovery"))
            .unwrap();
        assert!(matches!(
            received.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn vendor_toggle_revision_fences_an_unobserved_disable_and_reenable() {
        let (sent, received) = mpsc::sync_channel(1);
        sent.try_send(KeepPeekEvent::TimelineEventEnded {
            id: "occupied".to_owned(),
            end_time_ms: 0,
        })
        .unwrap();
        let mut active = HashMap::from([("person".to_owned(), "old-person".to_owned())]);
        let mut gate = Gate::default();
        let old_policy = Policy {
            enabled: true,
            revision: 10,
            ..Policy::default()
        };
        gate.update(old_policy);
        gate.logged_in();
        gate.subscribe(0, |_| Ok(())).unwrap();
        let new_policy = Policy {
            enabled: true,
            revision: 12,
            ..Policy::default()
        };
        assert!(!gate.permits(new_policy));

        gate.reconcile(new_policy, 123, &mut active, &sent);

        assert!(!gate.permits(new_policy));
        assert_eq!(active.get("person").map(String::as_str), Some("old-person"));
        received.try_recv().unwrap();
        gate.reconcile(new_policy, 456, &mut active, &sent);
        assert!(gate.permits(new_policy));
        assert!(!gate.permits(old_policy));
        assert!(matches!(
            received.try_recv().unwrap(),
            KeepPeekEvent::TimelineEventEnded { id, end_time_ms: 123 } if id == "old-person"
        ));
    }

    #[test]
    fn ending_retries_have_a_per_tick_budget_at_the_pending_limit() {
        let (sent, received) = mpsc::sync_channel(128);
        let mut active: HashMap<_, _> = (0..128)
            .map(|index| (format!("kind-{index}"), format!("event-{index}")))
            .collect();

        assert!(!try_end_events(&mut active, 123, &sent));

        assert_eq!(active.len(), 124);
        assert_eq!(received.try_iter().count(), 4);
    }

    #[test]
    fn failed_subscription_enqueue_does_not_grant_permission_or_prevent_retry() {
        let mut gate = Gate::default();
        let policy = Policy {
            enabled: true,
            revision: 1,
            ..Policy::default()
        };
        gate.update(policy);
        gate.logged_in();

        assert!(
            gate.subscribe(0, |_| {
                Err(reo_proto::BcError::XmlParse("enqueue rejected"))
            })
            .is_err()
        );
        assert!(!gate.permits(policy));

        let mut commands = Vec::new();
        gate.subscribe(0, |command| {
            commands.push(command);
            Ok(())
        })
        .unwrap();

        assert!(gate.permits(policy));
        assert!(matches!(
            commands.as_slice(),
            [Command::Alarm(AlarmCommand::StartMotionAlarm {
                channel: 0
            })]
        ));
        gate.subscribe(0, |_| panic!("successful subscription must not repeat"))
            .unwrap();
    }

    #[test]
    fn vendor_toggle_queues_one_real_protocol_packet_on_the_live_session() {
        let mut session = BcSession::default_client(Instant::now());
        session.set_state(reo_proto::SessionState::Connected);
        let mut gate = Gate::default();
        gate.logged_in();
        gate.update(Policy {
            enabled: false,
            revision: 1,
            ..Policy::default()
        });
        gate.subscribe(2, |command| session.handle_input(Input::Command(command)))
            .unwrap();
        let mut output = [0u8; 4096];
        assert!(matches!(
            session.poll_output(&mut output).unwrap(),
            Output::Timeout(_)
        ));

        gate.update(Policy {
            enabled: true,
            revision: 2,
            ..Policy::default()
        });
        for _ in 0..3 {
            gate.subscribe(2, |command| session.handle_input(Input::Command(command)))
                .unwrap();
        }

        match session.poll_output(&mut output).unwrap() {
            Output::TcpSend { data } => {
                let (header, _) = reo_proto::PacketHeader::parse(data).unwrap();
                assert_eq!(header.msg_id, reo_proto::COMMAND_START_MOTION_ALARM);
            }
            output => panic!("expected a motion subscription packet, got {output:?}"),
        }
        assert!(matches!(
            session.poll_output(&mut output).unwrap(),
            Output::Timeout(_)
        ));
        assert_eq!(session.state(), reo_proto::SessionState::Connected);
    }

    #[test]
    fn full_event_channel_does_not_block_protocol_keepalive() {
        let (sent, received) = mpsc::sync_channel(1);
        sent.try_send(KeepPeekEvent::TimelineEventEnded {
            id: "occupied".to_owned(),
            end_time_ms: 0,
        })
        .unwrap();
        let (completed, completion) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let now = Instant::now();
            let mut session = BcSession::new(
                BcSessionConfig {
                    keepalive_interval: Duration::from_secs(5),
                    ..BcSessionConfig::default_client()
                },
                now,
            );
            session.set_state(reo_proto::SessionState::Connected);
            let mut gate = Gate::default();
            gate.update(Policy {
                enabled: true,
                revision: 1,
                ..Policy::default()
            });
            gate.logged_in();
            gate.subscribe(0, |_| Ok(())).unwrap();
            let mut active = HashMap::from([("person".to_owned(), "old-person".to_owned())]);

            gate.reconcile(
                Policy {
                    enabled: false,
                    revision: 2,
                    ..Policy::default()
                },
                123,
                &mut active,
                &sent,
            );
            session
                .handle_input(Input::Timeout(now + Duration::from_secs(6)))
                .unwrap();

            let mut output = [0u8; 256];
            let Output::TcpSend { data } = session.poll_output(&mut output).unwrap() else {
                panic!("expected a keepalive packet");
            };
            let (header, _) = reo_proto::PacketHeader::parse(data).unwrap();
            completed.send((header.msg_id, active.len())).unwrap();
        });
        let result = completion.recv_timeout(Duration::from_secs(1));
        drop(received);
        worker.join().unwrap();

        assert_eq!(
            result.expect("event backpressure must not block keepalive"),
            (reo_proto::COMMAND_PING, 1)
        );
    }

    #[test]
    fn snapshot_invalidation_does_not_attach_an_old_reply_to_new_events() {
        let mut pending = VecDeque::from([
            vec!["old-in-flight".to_owned()],
            vec!["old-waiting".to_owned()],
        ]);

        invalidate_snapshots(&mut pending, true);
        pending.push_back(vec!["new-event".to_owned()]);

        assert!(pending.pop_front().unwrap().is_empty());
        assert_eq!(pending.pop_front().unwrap(), ["new-event"]);
        assert!(pending.is_empty());
    }

    #[test]
    fn snapshot_invalidation_clears_unstarted_requests_and_keeps_one_reply_slot() {
        let mut pending = VecDeque::from([vec!["old-event".to_owned()]]);
        invalidate_snapshots(&mut pending, false);
        assert!(pending.is_empty());

        pending.push_back(vec!["in-flight".to_owned()]);
        invalidate_snapshots(&mut pending, true);
        pending.push_back(vec!["new-waiting".to_owned()]);
        invalidate_snapshots(&mut pending, true);
        assert_eq!(pending, VecDeque::from([Vec::<String>::new()]));
    }

    #[test]
    fn vendor_toggle_observed_levels_close_events_with_a_stable_revision() {
        let (sent, received) = mpsc::sync_channel(1);
        let mut active = HashMap::from([("person".to_owned(), "old-person".to_owned())]);
        let mut gate = Gate::default();
        let enabled = Policy {
            enabled: true,
            revision: 1,
            ..Policy::default()
        };
        gate.update(enabled);
        gate.logged_in();
        gate.subscribe(0, |_| Ok(())).unwrap();
        let disabled = Policy {
            enabled: false,
            ..enabled
        };

        gate.reconcile(disabled, 123, &mut active, &sent);

        assert!(!gate.permits(disabled));
        assert!(!gate.permits(enabled));
        assert!(active.is_empty());
        assert!(matches!(received.try_recv().unwrap(),
            KeepPeekEvent::TimelineEventEnded { id, end_time_ms: 123 } if id == "old-person"));
        gate.reconcile(enabled, 456, &mut active, &sent);
        assert!(gate.permits(enabled));
        gate.subscribe(0, |_| panic!("must reuse the live subscription"))
            .unwrap();
        assert!(matches!(
            received.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn current_motion_mask_fences_old_events_and_filters_new_alarm_kinds() {
        let (sent, received) = mpsc::sync_channel(1);
        let mut active = HashMap::from([("motion".to_owned(), "old-motion".to_owned())]);
        let mut gate = Gate::default();
        let policy = Policy {
            enabled: true,
            revision: 1,
            record_motion: true,
        };
        gate.update(policy);
        gate.logged_in();
        gate.subscribe(0, |_| Ok(())).unwrap();
        let filtered = Policy {
            record_motion: false,
            ..policy
        };
        assert!(!gate.permits(filtered));

        gate.reconcile(filtered, 123, &mut active, &sent);

        assert!(gate.permits(filtered));
        assert!(!gate.permits(policy));
        assert!(active.is_empty());
        assert!(matches!(received.try_recv().unwrap(),
            KeepPeekEvent::TimelineEventEnded { id, end_time_ms: 123 } if id == "old-motion"));
        let alarm = reo_proto::alarm::AlarmEventData {
            status: "MD".try_into().unwrap(),
            ai_types: "people".try_into().unwrap(),
            ..Default::default()
        };
        assert_eq!(
            super::super::alarm_event_kinds(&alarm, filtered.record_motion),
            ["person"]
        );
    }

    #[test]
    fn vendor_toggle_registry_modes_hot_enable_the_same_protocol_session() {
        for mode in [
            EventMode::Disabled,
            EventMode::OnvifPullpoint,
            EventMode::RtspMetadata,
        ] {
            let (sent, _received) = mpsc::sync_channel(1);
            let camera = camera_loop(sent);
            configure_events(&camera, mode);
            let mut gate = Gate::default();
            let mut active = HashMap::new();
            let mut session = BcSession::default_client(Instant::now());
            session.set_state(reo_proto::SessionState::Connected);
            let mut output = [0u8; 4096];
            camera.poll_event_policy(&mut gate, &mut active, &mut session);
            gate.logged_in();
            camera.poll_event_policy(&mut gate, &mut active, &mut session);
            assert!(matches!(
                session.poll_output(&mut output).unwrap(),
                Output::Timeout(_)
            ));

            configure_events(&camera, EventMode::Vendor);
            camera.poll_event_policy(&mut gate, &mut active, &mut session);

            match session.poll_output(&mut output).unwrap() {
                Output::TcpSend { data } => {
                    let (header, _) = reo_proto::PacketHeader::parse(data).unwrap();
                    assert_eq!(header.msg_id, reo_proto::COMMAND_START_MOTION_ALARM);
                }
                output => panic!("expected a subscription after {mode:?}, got {output:?}"),
            }
            camera.poll_event_policy(&mut gate, &mut active, &mut session);
            assert!(matches!(
                session.poll_output(&mut output).unwrap(),
                Output::Timeout(_)
            ));
            assert_eq!(session.state(), reo_proto::SessionState::Connected);
            assert!(gate.permits(camera.event_policy()));
        }
    }

    #[test]
    fn vendor_toggle_registry_revision_detects_changes_between_loop_polls() {
        let (sent, received) = mpsc::sync_channel(1);
        let camera = camera_loop(sent);
        configure_events(&camera, EventMode::Vendor);
        let mut gate = Gate::default();
        gate.logged_in();
        let mut active = HashMap::new();
        let mut session = BcSession::default_client(Instant::now());
        session.set_state(reo_proto::SessionState::Connected);
        camera.poll_event_policy(&mut gate, &mut active, &mut session);
        active.insert("person".to_owned(), "old-person".to_owned());
        camera
            .tx
            .try_send(KeepPeekEvent::TimelineEventEnded {
                id: "occupied".to_owned(),
                end_time_ms: 0,
            })
            .unwrap();

        configure_events(&camera, EventMode::Disabled);
        configure_events(&camera, EventMode::Vendor);
        camera.poll_event_policy(&mut gate, &mut active, &mut session);

        assert!(!gate.permits(camera.event_policy()));
        assert_eq!(active.get("person").map(String::as_str), Some("old-person"));
        received.try_recv().unwrap();
        camera.poll_event_policy(&mut gate, &mut active, &mut session);
        assert!(active.is_empty());
        assert!(gate.permits(camera.event_policy()));
        assert!(matches!(received.try_recv().unwrap(),
            KeepPeekEvent::TimelineEventEnded { id, .. } if id == "old-person"));
        camera.poll_event_policy(&mut gate, &mut active, &mut session);
        assert!(matches!(
            received.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn incoming_alarm_endings_keep_channel_filtering_and_inactive_semantics() {
        let (sent, received) = mpsc::sync_channel(4);
        let camera = camera_loop(sent);
        let mut active = HashMap::new();
        let alarm = reo_proto::alarm::AlarmEventData {
            channel: 2,
            status: "MD".try_into().unwrap(),
            ai_types: "people".try_into().unwrap(),
            ..Default::default()
        };
        let started = camera.record_alarm_events(&[alarm], false, &mut active);
        assert_eq!(started.len(), 1);
        assert!(matches!(received.try_recv().unwrap(),
            KeepPeekEvent::TimelineEventStarted { event }
                if event.id == started[0] && event.kind == "person"));
        let inactive = reo_proto::alarm::AlarmEventData {
            channel: 3,
            status: "none".try_into().unwrap(),
            ..Default::default()
        };
        camera.record_alarm_events(std::slice::from_ref(&inactive), false, &mut active);
        assert_eq!(active.len(), 1);
        assert!(matches!(
            received.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        let inactive = reo_proto::alarm::AlarmEventData {
            channel: 2,
            ..inactive
        };
        camera.record_alarm_events(std::slice::from_ref(&inactive), false, &mut active);

        assert!(active.is_empty());
        assert!(matches!(received.try_recv().unwrap(),
            KeepPeekEvent::TimelineEventEnded { id, .. } if id == started[0]));
        camera.record_alarm_events(&[inactive], false, &mut active);
        assert!(matches!(
            received.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }
}
