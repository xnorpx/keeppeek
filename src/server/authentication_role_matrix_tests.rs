use super::super::{ServerControlHandler, control_request, proto};
use super::*;
use crate::access::AccessRole;
use crate::webrtc::{ControlRequestHandler, SessionId};

fn external_state(proxy: bool, role: AccessRole) -> (ServerState, String, String) {
    let (state, cookie, csrf) = tests::browser_state();
    let mut registry = state.authentication.lock().unwrap();
    let identity = &mut registry.directory.records[0];
    identity.role = role;
    if role == AccessRole::Administrator {
        identity.camera_access = CameraAccess::unrestricted();
    }
    if proxy {
        identity.subject_fingerprint = external::subject_fingerprint("proxy:company", "alice");
    }
    let provider = &mut registry.config.as_mut().unwrap().providers[0];
    provider.mappings[0].role = role;
    if role == AccessRole::Administrator {
        provider.mappings[0].camera_access = None;
    }
    if proxy {
        provider.mappings[0].claim = "role".into();
        provider.method = external::Method::Proxy(toml::from_str(
            "trusted_peers = ['203.0.113.1/32']\nsubject_header = 'X-Identity-Subject'\nrole_header = 'X-Identity-Role'",
        ).unwrap());
    }
    drop(registry);
    (state, cookie, csrf)
}

fn external_request(path: &str, cookie: &str, csrf: &str, proxy: bool) -> Request {
    let mut headers = vec![
        ("Host".into(), "keeppeek.example".into()),
        ("Origin".into(), "https://keeppeek.example".into()),
        ("Cookie".into(), format!("{SESSION_COOKIE}={cookie}")),
        ("X-KeepPeek-CSRF".into(), csrf.into()),
    ];
    if proxy {
        headers.extend([
            ("X-Identity-Subject".into(), "alice".into()),
            ("X-Identity-Role".into(), "users".into()),
        ]);
    }
    Request::fake_https_from(
        "203.0.113.1:4567".parse().unwrap(),
        "GET",
        path,
        headers,
        vec![],
    )
}

#[test]
fn external_oidc_and_proxy_roles_enforce_http_logs_and_revoke() {
    for proxy in [false, true] {
        for role in [AccessRole::User, AccessRole::Administrator] {
            let (state, cookie, csrf) = external_state(proxy, role);
            let (_router, router_tx) = crate::runtime::Router::new().unwrap();
            for revoked in [false, true] {
                for path in ["/logs", "/logs/snapshot", "/config/export"] {
                    let response = super::super::handle_request(
                        &external_request(path, &cookie, &csrf, proxy),
                        &router_tx,
                        &state,
                    );
                    let expected = if revoked {
                        401
                    } else if role == AccessRole::User {
                        403
                    } else {
                        503
                    };
                    assert_eq!(
                        response.status_code, expected,
                        "proxy={proxy} {role:?} {path}"
                    );
                }
                let mut registry = state.authentication.lock().unwrap();
                if let Some(browser) =
                    registry
                        .sessions
                        .lookup(&cookie, "https://keeppeek.example", Instant::now())
                {
                    registry.sessions.revoke(browser.id);
                }
            }
        }
    }
}

fn control_commands() -> Vec<(&'static str, control_request::Command, AccessRole)> {
    use control_request::Command as C;
    let mut commands = vec![
        (
            "health",
            C::HealthCommand(proto::HealthCommand::default()),
            AccessRole::Administrator,
        ),
        (
            "settings",
            C::ConfigurationCommand(proto::ConfigurationCommand::default()),
            AccessRole::Administrator,
        ),
        (
            "state",
            C::StateStoreCommand(proto::StateStoreCommand {
                action: Some(proto::state_store_command::Action::Get(proto::GetState {
                    namespace: "keeppeek.ui".into(),
                    key: "layout".into(),
                })),
            }),
            AccessRole::User,
        ),
        (
            "stored_media",
            C::StoredMediaCommand(proto::StoredMediaCommand {
                action: Some(proto::stored_media_command::Action::Open(
                    proto::OpenStoredMedia {
                        source_id: "front".into(),
                        ..Default::default()
                    },
                )),
            }),
            AccessRole::User,
        ),
        (
            "ptz",
            C::CameraControlCommand(proto::CameraControlCommand {
                action: Some(proto::camera_control_command::Action::Ptz(
                    proto::PtzCommand {
                        source_id: "front".into(),
                        ..Default::default()
                    },
                )),
            }),
            AccessRole::User,
        ),
        (
            "talk",
            C::TalkbackCommand(proto::TalkbackCommand::default()),
            AccessRole::Administrator,
        ),
        (
            "export",
            C::ExportCommand(proto::ExportCommand::default()),
            AccessRole::Administrator,
        ),
    ];
    commands.extend(restricted_commands());
    commands
}

fn restricted_commands() -> Vec<(&'static str, control_request::Command, AccessRole)> {
    use control_request::Command as C;
    vec![
        (
            "restricted_state",
            C::StateStoreCommand(proto::StateStoreCommand {
                action: Some(proto::state_store_command::Action::Get(proto::GetState {
                    namespace: "keeppeek.integrations.mqtt".into(),
                    key: "settings".into(),
                })),
            }),
            AccessRole::Administrator,
        ),
        (
            "ungranted_media",
            C::StoredMediaCommand(proto::StoredMediaCommand {
                action: Some(proto::stored_media_command::Action::Open(
                    proto::OpenStoredMedia {
                        source_id: "back".into(),
                        ..Default::default()
                    },
                )),
            }),
            AccessRole::Administrator,
        ),
        (
            "ungranted_ptz",
            C::CameraControlCommand(proto::CameraControlCommand {
                action: Some(proto::camera_control_command::Action::Ptz(
                    proto::PtzCommand {
                        source_id: "back".into(),
                        ..Default::default()
                    },
                )),
            }),
            AccessRole::Administrator,
        ),
    ]
}

#[test]
fn external_oidc_and_proxy_roles_enforce_rtc_matrix_and_revoke() {
    for proxy in [false, true] {
        for role in [AccessRole::User, AccessRole::Administrator] {
            let (state, cookie, csrf) = external_state(proxy, role);
            let principal = super::super::api_principal(
                &external_request("/create", &cookie, &csrf, proxy),
                &state,
            )
            .unwrap()
            .principal;
            let ApiPrincipalIdentity::External { browser, .. } = principal.identity else {
                panic!("matrix must exercise an external principal");
            };
            let session = SessionId::from_u64(700);
            let mut record = super::super::tests::local_test_session();
            record.principal = principal;
            record.classification = state
                .network_access
                .classify("203.0.113.1".parse().unwrap(), std::iter::empty());
            state
                .api_session_owners
                .lock()
                .unwrap()
                .insert(session, record);
            let (_router, router_tx) = crate::runtime::Router::new().unwrap();
            let handler = ServerControlHandler::new(state, router_tx);
            for revoked in [false, true] {
                check_commands(&handler, session, role, revoked);
                handler
                    .state
                    .authentication
                    .lock()
                    .unwrap()
                    .sessions
                    .revoke(browser);
            }
        }
    }
}

fn check_commands(
    handler: &ServerControlHandler,
    session: SessionId,
    role: AccessRole,
    revoked: bool,
) {
    for (name, command, required) in control_commands() {
        let result = handler.authorize_session_command(
            session,
            &proto::Request {
                request_id: 1,
                command: Some(command),
            },
        );
        if !revoked && role.permits(required) {
            assert!(result.is_ok(), "{role:?} {name}");
        } else {
            let error = result.expect_err("command must be denied");
            assert_eq!(error.code, proto::ErrorCode::Rejected, "{role:?} {name}");
            if !revoked {
                assert!(!error.close_session, "{role:?} {name}");
            }
        }
    }
    if revoked {
        assert!(
            !handler
                .state
                .api_session_owners
                .lock()
                .unwrap()
                .contains_key(&session)
        );
    }
}
