use super::*;

fn restricted_queue(state: &ServerState, session_id: SessionId) -> ApiEventQueue {
    let issued = restricted_test_user(state);
    state
        .access_manager
        .set_camera_access(
            issued.metadata.id,
            issued.metadata.revision,
            crate::access::CameraAccess {
                all_cameras: false,
                group_ids: Vec::new(),
                camera_ids: vec!["native-front".to_owned()],
            },
        )
        .unwrap();
    bind_credential_test_session(state, session_id, issued.access_key);
    event_queue(state, session_id, 8)
}

#[test]
fn cached_native_delivery_rechecks_absolute_and_idle_session_expiry() {
    for absolute in [true, false] {
        let state = live_state();
        let session_id = SessionId::from_u64(113);
        let queue = restricted_queue(&state, session_id);
        state.publish_camera_event(&native_event("digital_input"));
        test_control_handler(state.clone())
            .initial_capabilities(session_id)
            .unwrap();
        subscribe(&state, session_id, "restricted-events");
        state.publish_camera_event(&native_event("digital_input"));
        assert_eq!(queue.drain().len(), 1);
        {
            let mut sessions = state.api_session_owners.lock().unwrap();
            let session = sessions.get_mut(&session_id).unwrap();
            if absolute {
                session.absolute_expires_at_ms = 0;
            } else {
                session.last_activity =
                    Instant::now() - state.api_session_policy.idle_timeout - Duration::from_secs(1);
            }
        }
        assert!(camera_access::for_session(&state, session_id).is_err());
        state.publish_camera_event(&native_event("digital_input"));
        assert!(
            queue.drain().is_empty(),
            "expired sessions cannot use cached capabilities"
        );
        assert_eq!(state.event_subscriptions.metrics_snapshot().active, 0);
    }
}

#[test]
fn native_refresh_excludes_cameras_outside_the_receiving_users_access() {
    let state = live_state();
    let mut other = state.camera_entries().remove(0);
    other.info.id = "private-back".to_owned();
    other.info.ip = "192.0.2.2".to_owned();
    other.configuration.ip = "192.0.2.2".parse().unwrap();
    publish_video(&state, other.configuration.ip);
    state.upsert_camera(other);
    let session_id = SessionId::from_u64(114);
    let queue = restricted_queue(&state, session_id);
    let handler = test_control_handler(state.clone());
    let initial = handler.initial_capabilities(session_id).unwrap();
    assert_eq!(initial.cameras.len(), 1);
    assert_eq!(initial.source_sessions.len(), 2);
    assert_eq!(initial.stored_media_sources.len(), 1);
    subscribe(&state, session_id, "restricted-events");
    state.publish_camera_event(&native_event("digital_input"));
    let notifications = queue.drain();
    assert_eq!(notifications.len(), 2);
    let refreshed = snapshot(&notifications[0]);
    assert_eq!(refreshed.cameras.len(), 1);
    assert_eq!(refreshed.cameras[0].source_id, "native-front");
    assert_eq!(refreshed.source_sessions.len(), 2);
    assert_eq!(refreshed.source_sessions[1].source_id, "native-front");
    assert_eq!(refreshed.stored_media_sources.len(), 1);
    assert_eq!(refreshed.stored_media_sources[0].source_id, "native-front");
    assert_eq!(
        refreshed.self_source_session_id,
        format!("webrtc-client-{session_id}")
    );
    assert_eq!(
        refreshed.access_session.as_ref().unwrap().role,
        proto::AccessRole::User as i32
    );
    assert_eq!(delivered_event(&notifications[1]).source_id, "native-front");
}
