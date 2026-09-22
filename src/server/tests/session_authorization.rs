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

fn recording_storage() -> (std::path::PathBuf, crate::storage::StorageEngine) {
    use crate::storage::StorageEngine;
    let root = std::env::temp_dir().join(format!(
        "keeppeek-control-auth-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir(&root).unwrap();
    let engine = StorageEngine::start(StorageConfig {
        medium_term_path: root.clone(),
        long_term_path: root.clone(),
        recording_catalog_path: root.join("catalog.db"),
        event_thumbnail_path: root.join("thumbnails"),
        minimum_free_bytes: 0,
        warning_free_bytes: 0,
        critical_free_bytes: 0,
        long_term_max_bytes: 0,
        ..StorageConfig::default()
    });
    (root, engine)
}

#[test]
fn recording_controls_reject_users_and_revoked_administrators_before_mutation() {
    let (root, engine) = recording_storage();
    let mut state = media_test_state();
    let storage = engine.handle();
    storage.configure_camera_recording(
        "camera",
        crate::cameras::CameraRecordingMode::Sub,
        Duration::from_secs(60),
    );
    state.recording_control = Some(storage.clone());
    let original = storage.recording_control("camera").unwrap();
    let revision = format!(
        "{:032x}:{}",
        original.revision.epoch, original.revision.sequence
    );
    for role in [AccessRole::User, AccessRole::Administrator] {
        let issued = state
            .access_manager
            .create_credential(&format!("control {role:?}"), None, role, None, 1_000)
            .unwrap();
        let session = LiveApiSession::new(&state.webrtc);
        bind_credential_test_session(&state, session.id, issued.access_key);
        if role == AccessRole::Administrator {
            invalidate_credential(&state, &issued, true);
        }
        let handler = test_control_handler(state.clone());
        for action in recording_actions(&revision) {
            let request = proto::Request {
                request_id: 42,
                command: Some(control_request::Command::RecordingPolicyCommand(
                    proto::RecordingPolicyCommand {
                        source_id: "camera".into(),
                        action: Some(action),
                    },
                )),
            };
            let denied = session.request(&handler, request);
            assert_rejected(
                &denied,
                if role == AccessRole::User {
                    "Administrator role is required for this operation"
                } else {
                    "API session expired or was revoked"
                },
            );
            assert_eq!(storage.recording_control("camera").unwrap(), original);
            if role == AccessRole::Administrator {
                break;
            }
        }
    }
    engine.shutdown();
    std::fs::remove_dir_all(root).unwrap();
}

fn recording_actions(revision: &str) -> [proto::recording_policy_command::Action; 3] {
    use proto::recording_policy_command::Action;
    [
        Action::SetOverride(proto::SetRecordingOverride {
            expected_revision: revision.into(),
            enabled: false,
            source: proto::RecordingOverrideSource::External as i32,
            reason: "inspection".into(),
            ttl_ms: 60_000,
        }),
        Action::ClearOverride(proto::ClearRecordingOverride {
            expected_revision: revision.into(),
        }),
        Action::Get(proto::GetRecordingControl {}),
    ]
}

#[test]
fn preservation_rejects_users_and_revoked_administrators_at_the_live_session_boundary() {
    use proto::preservation_command::Action;
    let state = media_test_state();
    for role in [AccessRole::User, AccessRole::Administrator] {
        let issued = state
            .access_manager
            .create_credential(&format!("preservation {role:?}"), None, role, None, 1_000)
            .unwrap();
        let session = LiveApiSession::new(&state.webrtc);
        bind_credential_test_session(&state, session.id, issued.access_key);
        if role == AccessRole::Administrator {
            invalidate_credential(&state, &issued, true);
        }
        let mutation = proto::MutatePreservation {
            expected_revision: Some(0),
            reason: "evidence".into(),
        };
        for action in [
            Action::SaveForever(mutation.clone()),
            Action::Release(mutation),
            Action::Get(proto::GetPreservation {}),
        ] {
            let request = proto::Request {
                request_id: 42,
                command: Some(control_request::Command::PreservationCommand(
                    proto::PreservationCommand {
                        target: Some(proto::PreservationTarget {
                            target: Some(proto::preservation_target::Target::RecordingId(
                                "media".into(),
                            )),
                        }),
                        action: Some(action),
                    },
                )),
            };
            let denied = session.request(&test_control_handler(state.clone()), request);
            assert_rejected(
                &denied,
                if role == AccessRole::User {
                    "Administrator role is required for this operation"
                } else {
                    "API session expired or was revoked"
                },
            );
            if role == AccessRole::Administrator {
                break;
            }
        }
    }
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
