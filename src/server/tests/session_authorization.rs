use super::*;
use crate::webrtc::authorization_test_support::LiveApiSession;

fn read_session_request() -> proto::Request {
    proto::Request {
        request_id: 42,
        command: Some(control_request::Command::ServerCommand(
            proto::ServerCommand {
                action: Some(server_command::Action::GetAccessSession(
                    proto::GetAccessSession {},
                )),
            },
        )),
    }
}

fn invalidate_credential(state: &ServerState, issued: &IssuedCredential, revoke: bool) {
    // The command runs after credential invalidation and before bulk session cleanup.
    if revoke {
        state
            .access_manager
            .revoke_credential(issued.metadata.id, 3_000)
            .unwrap();
    } else {
        state
            .access_manager
            .set_camera_access(
                issued.metadata.id,
                issued.metadata.revision,
                crate::access::CameraAccess {
                    all_cameras: true,
                    group_ids: Vec::new(),
                    camera_ids: Vec::new(),
                },
            )
            .unwrap();
    }
}

fn assert_rejected(dispatch: &ControlDispatch, message: &str) {
    assert_eq!(dispatch.response.request_id, 42);
    let Some(control_response::Result::Error(error)) = &dispatch.response.result else {
        panic!("unauthorized request must fail");
    };
    assert_eq!(error.code, proto::ErrorCode::Rejected as i32);
    assert_eq!(error.message, message);
    assert!(dispatch.data_messages.is_empty());
    assert!(dispatch.notifications.is_empty());
}

#[test]
fn invalidated_control_session_closes_only_after_its_rejection_is_sent() {
    for revoke in [false, true] {
        let state = media_test_state();
        let issued = restricted_test_user(&state);
        let session = LiveApiSession::new(&state.webrtc);
        bind_credential_test_session(&state, session.id, issued.access_key);
        let handler = test_control_handler(state.clone());
        let accepted = session.request(&handler, read_session_request());
        assert!(matches!(
            accepted.response.result,
            Some(control_response::Result::Ok(_))
        ));
        if let Some(action) = accepted.after_send {
            action();
        }
        assert!(state.webrtc.has_api_session(session.id));
        invalidate_credential(&state, &issued, revoke);

        let rejected = session.request(&handler, read_session_request());
        assert_rejected(&rejected, "API session expired or was revoked");
        assert!(
            !state
                .api_session_owners
                .lock()
                .unwrap()
                .contains_key(&session.id)
        );
        assert!(
            state.webrtc.has_api_session(session.id),
            "response must precede closure"
        );
        rejected
            .after_send
            .expect("terminal authorization must schedule closure")();
        session.assert_closed();
    }
}

#[test]
fn invalidated_data_session_closes_its_live_transport() {
    for revoke in [false, true] {
        let state = media_test_state();
        let issued = restricted_test_user(&state);
        let session = LiveApiSession::new(&state.webrtc);
        bind_credential_test_session(&state, session.id, issued.access_key);
        let handler = test_control_handler(state.clone());
        invalidate_credential(&state, &issued, revoke);

        let error = session.data(&handler);
        assert_eq!(error.code, proto::ErrorCode::Rejected);
        assert_eq!(error.message, "API session expired or was revoked");
        assert!(
            !state
                .api_session_owners
                .lock()
                .unwrap()
                .contains_key(&session.id)
        );
        session.assert_closed();
    }
}

#[test]
fn valid_session_permission_denials_keep_the_live_transport_open() {
    let state = media_test_state();
    let issued = restricted_test_user(&state);
    let session = LiveApiSession::new(&state.webrtc);
    bind_credential_test_session(&state, session.id, issued.access_key);
    let handler = test_control_handler(state.clone());
    let mut request = read_session_request();
    request.command = Some(control_request::Command::RuntimeConfigurationCommand(
        proto::RuntimeConfigurationCommand {
            action: Some(runtime_configuration_command::Action::Get(
                proto::GetRuntimeConfiguration {},
            )),
        },
    ));
    let denied = session.request(&handler, request);
    assert_rejected(&denied, "Administrator role is required for this operation");
    assert!(denied.after_send.is_none());
    assert!(state.webrtc.has_api_session(session.id));

    let mut request = read_session_request();
    request.command = Some(control_request::Command::SubscribeMedia(media_request(
        proto::MediaKind::Video,
        proto::DeliveryTransport::Rtp,
        proto::VideoQuality::Auto,
        "",
    )));
    let denied = session.request(&handler, request);
    let Some(control_response::Result::Error(error)) = &denied.response.result else {
        panic!("ungranted camera must be denied");
    };
    assert_eq!(error.code, proto::ErrorCode::Rejected as i32);
    assert!(error.message.contains("camera access"));
    assert!(denied.after_send.is_none());
    assert!(state.webrtc.has_api_session(session.id));

    let error = session.data(&handler);
    assert_eq!(
        error.message,
        "Administrator role is required for this operation"
    );
    assert!(state.webrtc.has_api_session(session.id));
    assert!(
        state
            .api_session_owners
            .lock()
            .unwrap()
            .contains_key(&session.id)
    );
    let accepted = session.request(&handler, read_session_request());
    assert!(matches!(
        accepted.response.result,
        Some(control_response::Result::Ok(_))
    ));
}
