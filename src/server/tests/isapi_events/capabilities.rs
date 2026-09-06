use super::*;
use crate::webrtc::test_queue::ApiEventQueue;

mod guards;

fn live_state() -> ServerState {
    let state = media_test_state();
    {
        let mut cameras = state.cameras.write().unwrap();
        cameras[0].info.id = "native-front".to_owned();
        cameras[0].info.is_reolink = false;
        cameras[0].info.capabilities.events = false;
        cameras[0].info.capabilities.analytics = false;
    }
    publish_video(&state, Ipv4Addr::LOCALHOST.into());
    state
}

fn publish_video(state: &ServerState, camera_ip: IpAddr) {
    state.webrtc.live().publish(
        crate::webrtc::Source {
            camera_ip,
            stream: StreamKind::Sub,
        },
        crate::storage::VideoCodec::H264,
        true,
        Instant::now(),
        None,
        bytes::Bytes::from_static(&[0, 0, 0, 1]),
    );
}

#[test]
fn new_native_kind_queues_complete_snapshot_before_event() {
    let state = live_state();
    let session_id = SessionId::from_u64(96);
    let queue = ApiEventQueue::new(&state.webrtc, session_id, 4);
    let handler = test_control_handler(state.clone());
    let initial = handler.initial_capabilities(session_id).unwrap();
    assert_eq!(
        initial.source_sessions[1]
            .event_types
            .iter()
            .map(|kind| kind.event_type.as_str())
            .collect::<Vec<_>>(),
        ["person", "vehicle"]
    );
    assert!(
        !initial.cameras[0]
            .device_capabilities
            .as_ref()
            .unwrap()
            .events
    );
    state
        .event_subscriptions
        .subscribe(
            &state,
            session_id,
            proto::SubscribeEvents {
                subscription_id: "native-events".to_owned(),
                source_ids: vec!["native-front".to_owned()],
                ..Default::default()
            },
        )
        .unwrap();
    let mut event = timeline();
    event.kind = "digital_input".to_owned();

    state.publish_camera_event(&event);

    let notifications = queue.drain();
    assert_eq!(notifications.len(), 2);
    let Some(proto::notification::Event::InitialCapabilities(snapshot)) = &notifications[0].event
    else {
        panic!("the complete snapshot must precede the new event type");
    };
    assert_eq!(snapshot, &handler.initial_capabilities(session_id).unwrap());
    let source = snapshot
        .source_sessions
        .iter()
        .find(|source| source.source_id == "native-front")
        .unwrap();
    assert_eq!(source.event_types[0].event_type, "digital_input");
    assert!(source.video.is_some());
    let Some(proto::notification::Event::LiveEvent(delivered)) = &notifications[1].event else {
        panic!("the camera event must follow its capability snapshot");
    };
    assert_eq!(delivered.source_id, "native-front");
    assert_eq!(
        delivered.source_session_id.as_deref(),
        Some(source.source_session_id.as_str())
    );
    assert_eq!(delivered.origin, proto::EventOrigin::Camera as i32);
    assert_eq!(delivered.event_type, "digital_input");
}

fn subscribe(state: &ServerState, session_id: SessionId, subscription_id: &str) {
    state
        .event_subscriptions
        .subscribe(
            state,
            session_id,
            proto::SubscribeEvents {
                subscription_id: subscription_id.to_owned(),
                ..Default::default()
            },
        )
        .unwrap();
}

fn native_event(kind: &str) -> TimelineEvent {
    let mut event = timeline();
    event.kind = kind.to_owned();
    event
}

#[test]
fn capacity_one_sheds_the_subscription_without_sending_the_new_event() {
    let state = live_state();
    let session_id = SessionId::from_u64(97);
    let queue = ApiEventQueue::new(&state.webrtc, session_id, 1);
    subscribe(&state, session_id, "native-events");

    state.publish_camera_event(&native_event("digital_input"));

    let notifications = queue.drain();
    assert_eq!(notifications.len(), 1);
    assert!(matches!(
        notifications[0].event,
        Some(proto::notification::Event::InitialCapabilities(_))
    ));
    let metrics = state.event_subscriptions.metrics_snapshot();
    assert_eq!(metrics.deliveries, 1);
    assert_eq!(metrics.sheds, 1);
    assert_eq!(metrics.active, 0);
    assert_eq!(
        state
            .webrtc
            .api_event_queue_metrics_snapshot()
            .pending_bytes,
        0
    );
    assert!(queue.is_closed());
}

#[test]
fn a_full_snapshot_queue_sheds_without_enqueuing_an_event() {
    let state = live_state();
    let session_id = SessionId::from_u64(98);
    let queue = ApiEventQueue::new(&state.webrtc, session_id, 1);
    assert!(
        state
            .webrtc
            .try_enqueue_api_notification(session_id, proto::Notification::default())
            .unwrap()
    );
    subscribe(&state, session_id, "native-events");

    state.publish_camera_event(&native_event("digital_input"));

    assert_eq!(queue.drain(), [proto::Notification::default()]);
    assert_eq!(state.event_subscriptions.metrics_snapshot().sheds, 1);
    assert_eq!(state.event_subscriptions.metrics_snapshot().active, 0);
    assert_eq!(
        state
            .webrtc
            .api_event_queue_metrics_snapshot()
            .deliveries_queued,
        0
    );
}

#[test]
fn downstream_snapshot_backpressure_never_allows_the_dependent_event() {
    let state = live_state();
    let session_id = SessionId::from_u64(99);
    let queue = ApiEventQueue::new(&state.webrtc, session_id, 2);
    subscribe(&state, session_id, "native-events");
    state.publish_camera_event(&native_event("digital_input"));

    let notifications = queue.drain_with_control_capacity(256);

    assert!(notifications.is_empty());
    assert!(queue.is_closed());
    assert!(queue.drain().is_empty());
    assert_eq!(
        state
            .webrtc
            .api_event_queue_metrics_snapshot()
            .pending_bytes,
        0
    );
}

#[test]
fn metadata_only_camera_waits_for_an_advertised_live_source() {
    let state = media_test_state();
    let session_id = SessionId::from_u64(100);
    let queue = ApiEventQueue::new(&state.webrtc, session_id, 4);
    let handler = test_control_handler(state.clone());
    assert_eq!(
        handler
            .initial_capabilities(session_id)
            .unwrap()
            .source_sessions
            .len(),
        1
    );
    subscribe(&state, session_id, "native-events");

    state.publish_camera_event(&native_event("digital_input"));

    let notifications = queue.drain();
    assert_eq!(notifications.len(), 1);
    let Some(proto::notification::Event::InitialCapabilities(snapshot)) = &notifications[0].event
    else {
        panic!("metadata evidence must update the complete camera snapshot");
    };
    assert!(
        snapshot.cameras[0]
            .device_capabilities
            .as_ref()
            .unwrap()
            .events
    );
    assert_eq!(snapshot.source_sessions.len(), 1);
    assert!(snapshot.source_sessions[0].video.is_none());
    assert!(snapshot.source_sessions[0].audio.is_none());
    publish_video(&state, Ipv4Addr::LOCALHOST.into());

    state.publish_camera_event(&native_event("digital_input"));

    let notifications = queue.drain();
    assert_eq!(notifications.len(), 2);
    let Some(proto::notification::Event::InitialCapabilities(snapshot)) = &notifications[0].event
    else {
        panic!("newly active media must be advertised before live events");
    };
    assert!(snapshot.source_sessions[1].video.is_some());
    assert_eq!(
        snapshot.source_sessions[1].event_types[0].event_type,
        "digital_input"
    );
    let Some(proto::notification::Event::LiveEvent(event)) = &notifications[1].event else {
        panic!("an advertised native source must deliver its event");
    };
    assert_eq!(
        event.source_session_id.as_ref(),
        Some(&snapshot.source_sessions[1].source_session_id)
    );
}

fn snapshot(notification: &proto::Notification) -> &proto::ServerCapabilities {
    let Some(proto::notification::Event::InitialCapabilities(snapshot)) = &notification.event
    else {
        panic!("expected a complete capability snapshot");
    };
    snapshot
}

fn delivered_event(notification: &proto::Notification) -> &proto::Event {
    let Some(proto::notification::Event::LiveEvent(event)) = &notification.event else {
        panic!("expected a live event after its snapshot");
    };
    event
}

#[test]
fn complete_snapshots_keep_receiving_identity_and_unrelated_sources() {
    let state = live_state();
    let mut other_camera = state.camera_entries().remove(0);
    other_camera.info.id = "native-back".to_owned();
    other_camera.info.ip = "192.0.2.2".to_owned();
    other_camera.configuration.ip = "192.0.2.2".parse().unwrap();
    publish_video(&state, other_camera.configuration.ip);
    state.upsert_camera(other_camera);
    let session_ids = [SessionId::from_u64(101), SessionId::from_u64(102)];
    let queues = session_ids.map(|session_id| ApiEventQueue::new(&state.webrtc, session_id, 4));
    let observer = ApiEventQueue::new(&state.webrtc, SessionId::from_u64(103), 4);
    let handler = test_control_handler(state.clone());
    for session_id in session_ids {
        state
            .api_session_owners
            .lock()
            .unwrap()
            .insert(session_id, local_test_session());
        handler.initial_capabilities(session_id).unwrap();
        subscribe(&state, session_id, "native-events");
    }

    state.publish_camera_event(&native_event("digital_input"));

    for (session_id, queue) in session_ids.into_iter().zip(queues) {
        let notifications = queue.drain();
        assert_eq!(notifications.len(), 2);
        let snapshot = snapshot(&notifications[0]);
        assert_eq!(snapshot, &handler.initial_capabilities(session_id).unwrap());
        assert_eq!(
            snapshot.self_source_session_id,
            format!("webrtc-client-{session_id}")
        );
        assert_eq!(
            snapshot.access_session.as_ref().unwrap().session_id,
            session_id.to_string()
        );
        assert_eq!(snapshot.cameras.len(), 2);
        assert_eq!(snapshot.source_sessions.len(), 3);
        assert!(
            snapshot
                .source_sessions
                .iter()
                .any(|source| source.source_id == "native-back" && source.video.is_some())
        );
        assert_eq!(
            delivered_event(&notifications[1]).event_type,
            "digital_input"
        );
    }
    assert!(observer.drain().is_empty());
}

#[test]
fn a_known_kind_uses_the_initial_snapshot_and_is_not_resent_per_subscription() {
    let state = live_state();
    state.publish_camera_event(&native_event("digital_input"));
    let session_id = SessionId::from_u64(104);
    let queue = ApiEventQueue::new(&state.webrtc, session_id, 4);
    let handler = test_control_handler(state.clone());
    let initial = handler.initial_capabilities(session_id).unwrap();
    assert_eq!(
        initial.source_sessions[1].event_types[0].event_type,
        "digital_input"
    );
    subscribe(&state, session_id, "first-events");

    state.publish_camera_event(&native_event("digital_input"));

    let notifications = queue.drain();
    assert_eq!(notifications.len(), 1);
    assert_eq!(
        delivered_event(&notifications[0])
            .subscription_id
            .as_deref(),
        Some("first-events")
    );
    subscribe(&state, session_id, "second-events");

    state.publish_camera_event(&native_event("digital_input"));

    let notifications = queue.drain();
    assert_eq!(notifications.len(), 2);
    assert_eq!(
        delivered_event(&notifications[0])
            .subscription_id
            .as_deref(),
        Some("first-events")
    );
    assert_eq!(
        delivered_event(&notifications[1])
            .subscription_id
            .as_deref(),
        Some("second-events")
    );
}
