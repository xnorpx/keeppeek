use super::*;

#[test]
fn replacement_unsubscribe_and_close_cancel_queued_events() {
    let state = live_state();
    let session_id = SessionId::from_u64(105);
    let queue = ApiEventQueue::new(&state.webrtc, session_id, 4);
    subscribe(&state, session_id, "native-events");
    let mut event = native_event("digital_input");
    state.publish_camera_event(&event);
    subscribe(&state, session_id, "native-events");
    event.revision += 1;
    state.publish_camera_event(&event);

    let notifications = queue.drain();
    assert_eq!(notifications.len(), 2);
    snapshot(&notifications[0]);
    assert_eq!(delivered_event(&notifications[1]).revision, event.revision);
    state.publish_camera_event(&event);
    state
        .event_subscriptions
        .unsubscribe(session_id, &["native-events".to_owned()]);
    assert!(queue.drain().is_empty());
    subscribe(&state, session_id, "native-events");
    state.publish_camera_event(&event);
    test_control_handler(state.clone()).session_closed(session_id);
    assert!(queue.drain().is_empty());
    assert_eq!(state.event_subscriptions.metrics_snapshot().active, 0);
    assert_eq!(
        state
            .webrtc
            .api_event_queue_metrics_snapshot()
            .pending_bytes,
        0
    );
}

#[test]
fn evidence_recorded_before_publish_still_refreshes_the_subscriber() {
    let state = live_state();
    let session_id = SessionId::from_u64(106);
    let queue = ApiEventQueue::new(&state.webrtc, session_id, 4);
    test_control_handler(state.clone())
        .initial_capabilities(session_id)
        .unwrap();
    subscribe(&state, session_id, "native-events");
    assert!(
        state
            .health
            .events
            .record_kind(Ipv4Addr::LOCALHOST.into(), "digital_input")
    );

    state.publish_camera_event(&native_event("digital_input"));

    let notifications = queue.drain();
    assert_eq!(notifications.len(), 2);
    assert_eq!(
        snapshot(&notifications[0]).source_sessions[1].event_types[0].event_type,
        "digital_input"
    );
    assert_eq!(
        delivered_event(&notifications[1]).event_type,
        "digital_input"
    );
}

#[test]
fn software_origin_does_not_mutate_native_capabilities() {
    let state = live_state();
    let session_id = SessionId::from_u64(107);
    let queue = ApiEventQueue::new(&state.webrtc, session_id, 4);
    let handler = test_control_handler(state.clone());
    let initial = handler.initial_capabilities(session_id).unwrap();
    subscribe(&state, session_id, "native-events");
    let mut event = native_event("software_only");
    event.source = EventSource::KeepPeek;

    state.publish_camera_event(&event);

    assert!(queue.drain().is_empty());
    assert!(
        state
            .health
            .events
            .snapshot(Ipv4Addr::LOCALHOST.into())
            .is_none()
    );
    assert_eq!(handler.initial_capabilities(session_id).unwrap(), initial);
}

#[test]
fn disabled_policy_neither_learns_nor_delivers_a_native_kind() {
    let state = live_state();
    let session_id = SessionId::from_u64(108);
    let queue = ApiEventQueue::new(&state.webrtc, session_id, 4);
    subscribe(&state, session_id, "native-events");
    let policy = crate::cameras::events::EventConfig {
        mode: crate::cameras::events::EventMode::Disabled,
        ..Default::default()
    };
    state
        .health
        .events
        .configure(Ipv4Addr::LOCALHOST.into(), policy, false, Shutdown::new());

    state.publish_camera_event(&native_event("digital_input"));

    assert!(queue.drain().is_empty());
    assert!(
        state
            .health
            .events
            .snapshot(Ipv4Addr::LOCALHOST.into())
            .is_none()
    );
    let capabilities = test_control_handler(state)
        .initial_capabilities(session_id)
        .unwrap();
    assert!(
        !capabilities.cameras[0]
            .device_capabilities
            .as_ref()
            .unwrap()
            .events
    );
    assert_eq!(
        capabilities.source_sessions[1]
            .event_types
            .iter()
            .map(|kind| kind.event_type.as_str())
            .collect::<Vec<_>>(),
        ["person", "vehicle"]
    );
}

#[test]
fn an_oversized_complete_snapshot_sheds_instead_of_sending_a_partial_update() {
    let state = live_state();
    let session_id = SessionId::from_u64(109);
    let queue = ApiEventQueue::new(&state.webrtc, session_id, 4);
    subscribe(&state, session_id, "native-events");
    state.cameras.write().unwrap()[0].info.name =
        Some("x".repeat(crate::webrtc::MAX_CONTROL_MESSAGE_BYTES));

    state.publish_camera_event(&native_event("digital_input"));

    assert!(queue.drain().is_empty());
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
fn capability_updates_exclude_native_payloads_and_preserve_optional_isapi_images() {
    let state = live_state();
    let session_id = SessionId::from_u64(110);
    let queue = ApiEventQueue::new(&state.webrtc, session_id, 4);
    subscribe(&state, session_id, "native-events");
    let raw_url = "http://camera-user:private-password@camera.invalid/alert";
    let native_payload = "<EventNotificationAlert>private-native-body</EventNotificationAlert>";
    let mut event = native_event("license_plate");
    event.payload = serde_json::json!({"url": raw_url, "native": native_payload})
        .as_object()
        .cloned();

    state.publish_camera_event(&event);

    let notifications = queue.drain();
    assert_eq!(notifications.len(), 2);
    let snapshot = snapshot(&notifications[0]);
    let encoded = snapshot.encode_to_vec();
    let content = String::from_utf8_lossy(&encoded);
    assert!(!content.contains(raw_url));
    assert!(!content.contains(native_payload));
    let event_type = &snapshot.source_sessions[1].event_types[0];
    assert_eq!(event_type.event_type, "license_plate");
    assert!(event_type.metadata.is_none());
    assert_eq!(event_type.attachments[0].minimum_count, 0);
    assert_eq!(event_type.attachments[0].maximum_count, 16);
    assert!(delivered_event(&notifications[1]).payload.is_some());
}

#[test]
fn two_subscriptions_share_one_complete_update_for_a_new_kind() {
    let state = live_state();
    let session_id = SessionId::from_u64(111);
    let queue = ApiEventQueue::new(&state.webrtc, session_id, 4);
    subscribe(&state, session_id, "first-events");
    subscribe(&state, session_id, "second-events");

    state.publish_camera_event(&native_event("digital_input"));

    let notifications = queue.drain();
    assert_eq!(notifications.len(), 3);
    snapshot(&notifications[0]);
    assert_eq!(
        delivered_event(&notifications[1])
            .subscription_id
            .as_deref(),
        Some("first-events")
    );
    assert_eq!(
        delivered_event(&notifications[2])
            .subscription_id
            .as_deref(),
        Some("second-events")
    );
}

#[test]
fn concurrent_new_kinds_are_advertised_before_each_delivery() {
    let state = live_state();
    let session_id = SessionId::from_u64(112);
    let queue = ApiEventQueue::new(&state.webrtc, session_id, 8);
    subscribe(&state, session_id, "native-events");
    let barrier = std::sync::Barrier::new(3);
    std::thread::scope(|threads| {
        for kind in ["digital_input", "intrusion"] {
            let state = &state;
            let barrier = &barrier;
            threads.spawn(move || {
                barrier.wait();
                state.publish_camera_event(&native_event(kind));
            });
        }
        barrier.wait();
    });

    let notifications = queue.drain();
    assert_eq!(notifications.len(), 4);
    let mut advertised = HashSet::new();
    let mut delivered = HashSet::new();
    for notification in notifications {
        match notification.event.unwrap() {
            proto::notification::Event::InitialCapabilities(snapshot) => {
                advertised = snapshot
                    .source_sessions
                    .iter()
                    .flat_map(|source| {
                        source
                            .event_types
                            .iter()
                            .map(|kind| (source.source_session_id.clone(), kind.event_type.clone()))
                    })
                    .collect();
            }
            proto::notification::Event::LiveEvent(event) => {
                assert!(
                    advertised
                        .contains(&(event.source_session_id.unwrap(), event.event_type.clone()))
                );
                delivered.insert(event.event_type);
            }
            _ => panic!("unexpected notification during native discovery"),
        }
    }
    assert_eq!(
        delivered,
        HashSet::from(["digital_input".to_owned(), "intrusion".to_owned()])
    );
}
