use std::{
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
    sync::Arc,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use uuid::Uuid;

use crate::{
    access::{AccessRole, CameraAccess, ClientClassificationReason},
    api::proto::{
        self, cloud_notification_command, cloud_notification_result, event_search_command,
        ok as control_ok, request as control_request, response as control_response,
    },
    config::cloud_notifications::Settings,
    notifications::{
        cloud::{CloudSignalRuntime, SignalTransport},
        cloud_protocol::CANONICAL_BODY_BYTES,
        cloud_state::SignalDeliveryOutcome,
    },
    server::{
        ApiPrincipal, ServerState,
        tests::{local_test_session, test_control_handler, test_session_record},
    },
    storage::metadata::{EventAttachment, EventSource, TimelineEvent},
    webrtc::{ControlRequestHandler, SessionId},
};

const TEST_SEED: [u8; 32] = [0x44; 32];
const NVR_A: &str = "01020304-0506-0708-090a-0b0c0d0e0f10";
const NVR_B: &str = "f1f2f3f4-f5f6-f7f8-f9fa-fbfcfdfeff00";

struct AlwaysAcceptTransport;

impl SignalTransport for AlwaysAcceptTransport {
    fn send_signal(
        &self,
        _service_origin: &str,
        _body: &[u8; CANONICAL_BODY_BYTES],
    ) -> SignalDeliveryOutcome {
        SignalDeliveryOutcome::Accepted
    }
}

fn setup_server(
    name: &str,
    paired: bool,
) -> (
    PathBuf,
    ServerState,
    crate::notifications::Runtime,
    CloudSignalRuntime,
    crate::storage::RecordingCatalog,
) {
    let dir = std::env::temp_dir().join(format!("keeppeek-cloud-srv-{name}-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let config_path = dir.join("config.toml");
    std::fs::write(&config_path, "").unwrap();
    std::fs::write(
        dir.join("secrets.toml"),
        format!("CLOUD_KEY = \"{}\"\n", STANDARD.encode(TEST_SEED)),
    )
    .unwrap();
    let settings = if paired {
        Settings {
            revision: 0,
            enabled: false,
            image_previews_enabled: false,
            service_origin: Some("https://service.keeppeek.example".to_owned()),
            nvr_id: Some(NVR_A.to_owned()),
            signing_key_version: Some(1),
            signing_key: Some("{secret:CLOUD_KEY}".to_owned()),
        }
    } else {
        Settings::default()
    };
    let cloud = CloudSignalRuntime::open_with_transport(
        config_path.clone(),
        &dir.join("cloud-notifications.db"),
        settings,
        Arc::new(AlwaysAcceptTransport),
        1_000_000,
    )
    .unwrap();
    let notifications =
        crate::notifications::Runtime::open_with_cloud(&config_path, cloud.clone()).unwrap();
    let catalog = crate::storage::RecordingCatalog::open(&dir.join("recordings.db")).unwrap();
    let event_store =
        crate::storage::EventStore::new(catalog.handle(), &dir.join("events"), 10 * 1024 * 1024)
            .unwrap();
    let state = ServerState::empty()
        .with_camera_config_path(config_path)
        .with_event_store(event_store)
        .with_notifications(notifications.handle())
        .with_cloud_notifications(cloud.clone());
    (dir, state, notifications, cloud, catalog)
}

fn insert_session(state: &ServerState, session_id: SessionId, principal: ApiPrincipal) {
    state.api_session_owners.lock().unwrap().insert(
        session_id,
        test_session_record(
            principal,
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            ClientClassificationReason::DirectLocal,
        ),
    );
}

fn send_cloud(
    handler: &crate::server::ServerControlHandler,
    session_id: SessionId,
    action: cloud_notification_command::Action,
) -> proto::Response {
    handler
        .handle_for_session(
            session_id,
            proto::Request {
                request_id: 1,
                command: Some(control_request::Command::CloudNotificationCommand(
                    proto::CloudNotificationCommand {
                        action: Some(action),
                    },
                )),
            },
        )
        .response
}

#[test]
fn capability_gate_revision_conflicts_and_admin_authorization_matrix() {
    // Capability is absent when cloud runtime is not attached.
    let bare_state = ServerState::empty();
    let bare_session = SessionId::from_u64(1);
    bare_state
        .api_session_owners
        .lock()
        .unwrap()
        .insert(bare_session, local_test_session());
    let bare_handler = test_control_handler(bare_state);
    assert!(
        !bare_handler
            .initial_capabilities(bare_session)
            .unwrap()
            .capability_ids
            .contains(&super::CAPABILITY_ID.to_owned())
    );

    let (dir, state, runtime, _cloud, catalog) = setup_server("auth-matrix", true);
    let admin_session = SessionId::from_u64(10);
    let user_session = SessionId::from_u64(11);
    state
        .api_session_owners
        .lock()
        .unwrap()
        .insert(admin_session, local_test_session());

    let user_cred = state
        .access_manager
        .create_credential("Standard user", None, AccessRole::User, None, 1_000)
        .unwrap();
    let user_auth = state
        .access_manager
        .authenticate(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &[&format!("Bearer {}", user_cred.access_key.canonical())],
            1_000,
            std::time::Instant::now(),
        )
        .unwrap();
    insert_session(&state, user_session, ApiPrincipal::credential(user_auth));

    let handler = test_control_handler(state);
    assert!(
        handler
            .initial_capabilities(admin_session)
            .unwrap()
            .capability_ids
            .contains(&super::CAPABILITY_ID.to_owned())
    );

    // Non-admin user cannot inspect or mutate cloud notification settings or status.
    for action in [
        cloud_notification_command::Action::GetSettings(proto::GetCloudNotificationSettings {}),
        cloud_notification_command::Action::GetStatus(proto::GetCloudNotificationStatus {}),
        cloud_notification_command::Action::Test(proto::TestCloudNotification {}),
        cloud_notification_command::Action::UpdateSettings(
            proto::UpdateCloudNotificationSettings {
                expected_revision: 0,
                enabled: true,
                image_previews_enabled: false,
            },
        ),
    ] {
        let resp = send_cloud(&handler, user_session, action);
        assert!(matches!(
            resp.result,
            Some(control_response::Result::Error(ref err))
                if err.code == proto::ErrorCode::Rejected as i32
        ));
    }

    // Enabling image_previews while disabled must be rejected without mutating revision.
    let invalid_preview = send_cloud(
        &handler,
        admin_session,
        cloud_notification_command::Action::UpdateSettings(
            proto::UpdateCloudNotificationSettings {
                expected_revision: 0,
                enabled: false,
                image_previews_enabled: true,
            },
        ),
    );
    assert!(matches!(
        invalid_preview.result,
        Some(control_response::Result::Error(ref err))
            if err.code == proto::ErrorCode::InvalidRequest as i32
    ));

    // Valid update increments revision atomically and persists to config.toml without secrets.
    let updated = send_cloud(
        &handler,
        admin_session,
        cloud_notification_command::Action::UpdateSettings(
            proto::UpdateCloudNotificationSettings {
                expected_revision: 0,
                enabled: true,
                image_previews_enabled: false,
            },
        ),
    );
    let Some(control_response::Result::Ok(proto::Ok {
        result:
            Some(control_ok::Result::CloudNotificationResult(proto::CloudNotificationResult {
                result: Some(cloud_notification_result::Result::Settings(settings)),
            })),
    })) = updated.result
    else {
        panic!("expected updated CloudNotificationSettings");
    };
    assert_eq!(settings.revision, 1);
    assert!(settings.enabled);
    assert!(!settings.image_previews_enabled);

    // Stale expected_revision conflicts without altering persisted state.
    let stale = send_cloud(
        &handler,
        admin_session,
        cloud_notification_command::Action::UpdateSettings(
            proto::UpdateCloudNotificationSettings {
                expected_revision: 0,
                enabled: false,
                image_previews_enabled: false,
            },
        ),
    );
    assert!(matches!(
        stale.result,
        Some(control_response::Result::Error(ref err))
            if err.code == proto::ErrorCode::Rejected as i32
    ));

    // Test signal succeeds up to 3 times/hour, then fails with LOCAL_LIMIT.
    for attempt in 0..4 {
        let test_resp = send_cloud(
            &handler,
            admin_session,
            cloud_notification_command::Action::Test(proto::TestCloudNotification {}),
        );
        let Some(control_response::Result::Ok(proto::Ok {
            result:
                Some(control_ok::Result::CloudNotificationResult(proto::CloudNotificationResult {
                    result: Some(cloud_notification_result::Result::Test(res)),
                })),
        })) = test_resp.result
        else {
            panic!("expected CloudNotificationTestResult");
        };
        if attempt < 3 {
            assert!(res.accepted);
            assert_eq!(
                res.failure,
                proto::CloudNotificationFailureCode::None as i32
            );
        } else {
            assert!(!res.accepted);
            assert_eq!(
                res.failure,
                proto::CloudNotificationFailureCode::LocalLimit as i32
            );
            assert!(res.next_eligible_at.is_some());
        }
    }

    drop(handler);
    runtime.shutdown();
    catalog.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn paired_mobile_preview_resolution_enforces_authoritative_nvr_setting_binding_and_media_types() {
    let (dir, state, runtime, cloud, catalog) = setup_server("preview-matrix", true);
    let now_ms = super::unix_time_ms();
    let admin_session = SessionId::from_u64(20);
    let mobile_ok_session = SessionId::from_u64(21);
    let mobile_wrong_nvr_session = SessionId::from_u64(22);
    let standard_user_session = SessionId::from_u64(23);

    state
        .api_session_owners
        .lock()
        .unwrap()
        .insert(admin_session, local_test_session());

    let mobile_ok = state
        .access_manager
        .create_paired_mobile_credential("Phone A", None, AccessRole::User, NVR_A, None, now_ms)
        .unwrap();
    let mobile_wrong = state
        .access_manager
        .create_paired_mobile_credential("Phone B", None, AccessRole::User, NVR_B, None, now_ms)
        .unwrap();
    let standard = state
        .access_manager
        .create_credential("Desktop", None, AccessRole::User, None, now_ms)
        .unwrap();

    for (session_id, key) in [
        (mobile_ok_session, mobile_ok.access_key),
        (mobile_wrong_nvr_session, mobile_wrong.access_key),
        (standard_user_session, standard.access_key),
    ] {
        let auth = state
            .access_manager
            .authenticate(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                &[&format!("Bearer {}", key.canonical())],
                now_ms,
                std::time::Instant::now(),
            )
            .unwrap();
        insert_session(&state, session_id, ApiPrincipal::credential(auth));
    }

    // Seed three events: valid JPEG with thumbnail, JPEG retained-away, and unsupported GIF/video.
    let events = state.events.as_ref().unwrap();
    let thumb_path = dir.join("events").join("evt-still.jpg");
    std::fs::create_dir_all(thumb_path.parent().unwrap()).unwrap();
    std::fs::write(&thumb_path, b"\xff\xd8\xff\xd9").unwrap();

    for (id, ctype, thumb) in [
        ("evt-still", "image/jpeg", Some("evt-still.jpg".to_owned())),
        ("evt-retained", "image/jpeg", None),
        ("evt-animated", "image/gif", None),
        ("evt-video", "video/mp4", None),
    ] {
        events
            .insert(TimelineEvent {
                id: id.to_owned(),
                revision: 1,
                camera_id: "front-door".to_owned(),
                stream: None,
                source: EventSource::KeepPeek,
                kind: "person".to_owned(),
                start_time_ms: now_ms,
                end_time_ms: None,
                confidence: None,
                bbox: None,
                bbox_attachment_id: None,
                zone: None,
                text: None,
                payload: None,
                attachments: vec![EventAttachment {
                    id: format!("att-{id}"),
                    attachment_type: "snapshot".to_owned(),
                    content_type: ctype.to_owned(),
                    byte_len: Some(4),
                    ordinal: 0,
                    timestamp_ms: Some(now_ms),
                    text: None,
                }],
                canonical_attachment_id: (ctype == "image/jpeg").then(|| format!("att-{id}")),
                icon_key: "person".to_owned(),
                rejected_icon_key: None,
                thumbnail_filename: thumb,
            })
            .unwrap();
    }

    // Enable cloud notifications WITHOUT previews first.
    let handler = test_control_handler(state.clone());
    let _ = send_cloud(
        &handler,
        admin_session,
        cloud_notification_command::Action::UpdateSettings(
            proto::UpdateCloudNotificationSettings {
                expected_revision: 0,
                enabled: true,
                image_previews_enabled: false,
            },
        ),
    );

    let alias_still = cloud
        .enqueue_logical_event("log-still", "rule-1", "evt-still", "front-door", 1, now_ms)
        .unwrap()
        .unwrap()
        .cloud_event_id;
    let alias_retained = cloud
        .enqueue_logical_event(
            "log-retained",
            "rule-1",
            "evt-retained",
            "front-door",
            1,
            now_ms,
        )
        .unwrap()
        .unwrap()
        .cloud_event_id;
    let alias_gif = cloud
        .enqueue_logical_event("log-gif", "rule-1", "evt-animated", "front-door", 1, now_ms)
        .unwrap()
        .unwrap()
        .cloud_event_id;
    let alias_video = cloud
        .enqueue_logical_event("log-vid", "rule-1", "evt-video", "front-door", 1, now_ms)
        .unwrap()
        .unwrap()
        .cloud_event_id;

    // While image_previews_enabled == false, resolve AND FetchMedia fail generically for mobile.
    let disabled_resp = send_cloud(
        &handler,
        mobile_ok_session,
        cloud_notification_command::Action::Resolve(proto::ResolveCloudNotification {
            cloud_event_id: alias_still.to_vec(),
        }),
    );
    assert!(matches!(
        disabled_resp.result,
        Some(control_response::Result::Error(ref err))
            if err.code == proto::ErrorCode::NotFound as i32
                && err.message == super::GENERIC_PREVIEW_UNAVAILABLE
    ));

    let media_blocked = handler
        .handle_for_session(
            mobile_ok_session,
            proto::Request {
                request_id: 2,
                command: Some(control_request::Command::EventSearchCommand(
                    proto::EventSearchCommand {
                        action: Some(event_search_command::Action::FetchMedia(
                            proto::FetchEventSearchMedia {
                                transfer_id: "tx-1".to_owned(),
                                objects: Vec::new(),
                                channel: proto::DataChannelKind::ReliableData as i32,
                            },
                        )),
                    },
                )),
            },
        )
        .response;
    assert!(matches!(
        media_blocked.result,
        Some(control_response::Result::Error(ref err))
            if err.message == super::GENERIC_PREVIEW_UNAVAILABLE
    ));

    // Now enable image_previews_enabled = true.
    let _ = send_cloud(
        &handler,
        admin_session,
        cloud_notification_command::Action::UpdateSettings(
            proto::UpdateCloudNotificationSettings {
                expected_revision: 1,
                enabled: true,
                image_previews_enabled: true,
            },
        ),
    );

    // Wrong endpoint kind, wrong NVR, local admin, and unknown alias all get identical generic error.
    for (sess, id) in [
        (standard_user_session, alias_still.to_vec()),
        (mobile_wrong_nvr_session, alias_still.to_vec()),
        (admin_session, alias_still.to_vec()),
        (mobile_ok_session, vec![0x99; 16]),
    ] {
        let resp = send_cloud(
            &handler,
            sess,
            cloud_notification_command::Action::Resolve(proto::ResolveCloudNotification {
                cloud_event_id: id,
            }),
        );
        assert!(matches!(
            resp.result,
            Some(control_response::Result::Error(ref err))
                if err.code == proto::ErrorCode::NotFound as i32
                    && err.message == super::GENERIC_PREVIEW_UNAVAILABLE
        ));
    }

    // Authorized paired mobile resolves canonical still image cleanly.
    let ok_resp = send_cloud(
        &handler,
        mobile_ok_session,
        cloud_notification_command::Action::Resolve(proto::ResolveCloudNotification {
            cloud_event_id: alias_still.to_vec(),
        }),
    );
    let Some(control_response::Result::Ok(proto::Ok {
        result:
            Some(control_ok::Result::CloudNotificationResult(proto::CloudNotificationResult {
                result: Some(cloud_notification_result::Result::Context(ctx)),
            })),
    })) = ok_resp.result
    else {
        panic!("expected CloudNotificationContext for still image");
    };
    assert_eq!(ctx.event_id, "evt-still");
    assert_eq!(ctx.revision, 1);
    assert_eq!(
        ctx.canonical_attachment
            .as_ref()
            .map(|a| a.attachment_id.as_str()),
        Some("att-evt-still")
    );
    assert_eq!(
        ctx.image_availability,
        proto::EventImageAvailability::Available as i32
    );

    // Retained-away image reports Unavailable cleanly (preserving generic mobile notification).
    let retained_resp = send_cloud(
        &handler,
        mobile_ok_session,
        cloud_notification_command::Action::Resolve(proto::ResolveCloudNotification {
            cloud_event_id: alias_retained.to_vec(),
        }),
    );
    let Some(control_response::Result::Ok(proto::Ok {
        result:
            Some(control_ok::Result::CloudNotificationResult(proto::CloudNotificationResult {
                result: Some(cloud_notification_result::Result::Context(retained_ctx)),
            })),
    })) = retained_resp.result
    else {
        panic!("expected CloudNotificationContext for retained image");
    };
    assert_eq!(
        retained_ctx.image_availability,
        proto::EventImageAvailability::Unavailable as i32
    );

    // Unsupported animated GIF and video attachments yield None attachment + None availability.
    for alias in [alias_gif, alias_video] {
        let resp = send_cloud(
            &handler,
            mobile_ok_session,
            cloud_notification_command::Action::Resolve(proto::ResolveCloudNotification {
                cloud_event_id: alias.to_vec(),
            }),
        );
        let Some(control_response::Result::Ok(proto::Ok {
            result:
                Some(control_ok::Result::CloudNotificationResult(proto::CloudNotificationResult {
                    result: Some(cloud_notification_result::Result::Context(ctx)),
                })),
        })) = resp.result
        else {
            panic!("expected generic context without unsupported media");
        };
        assert!(ctx.canonical_attachment.is_none());
        assert_eq!(
            ctx.image_availability,
            proto::EventImageAvailability::None as i32
        );
    }

    // Restricting camera access bumps credential revision; re-authenticating enforces the
    // restricted camera set and denies resolution with the same generic error.
    state
        .access_manager
        .set_camera_access(
            mobile_ok.metadata.id,
            mobile_ok.metadata.revision,
            CameraAccess {
                all_cameras: false,
                group_ids: Vec::new(),
                camera_ids: vec!["back-door".to_owned()],
            },
        )
        .unwrap();
    let stale_cam = send_cloud(
        &handler,
        mobile_ok_session,
        cloud_notification_command::Action::Resolve(proto::ResolveCloudNotification {
            cloud_event_id: alias_still.to_vec(),
        }),
    );
    assert!(matches!(
        stale_cam.result,
        Some(control_response::Result::Error(_))
    ));
    let mobile_restricted_auth = state
        .access_manager
        .authenticate(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &[&format!("Bearer {}", mobile_ok.access_key.canonical())],
            now_ms,
            std::time::Instant::now(),
        )
        .unwrap();
    insert_session(
        &state,
        mobile_ok_session,
        ApiPrincipal::credential(mobile_restricted_auth),
    );
    let denied_cam = send_cloud(
        &handler,
        mobile_ok_session,
        cloud_notification_command::Action::Resolve(proto::ResolveCloudNotification {
            cloud_event_id: alias_still.to_vec(),
        }),
    );
    assert!(matches!(
        denied_cam.result,
        Some(control_response::Result::Error(ref err))
            if err.code == proto::ErrorCode::NotFound as i32
                && err.message == super::GENERIC_PREVIEW_UNAVAILABLE
    ));

    drop(handler);
    drop(state);
    runtime.shutdown();
    catalog.shutdown();
    std::fs::remove_dir_all(dir).unwrap();
}
