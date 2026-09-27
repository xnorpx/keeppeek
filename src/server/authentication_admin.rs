//! Provides bounded Administrator views and revocation for external identities.

use super::*;
use crate::access::AccessRole;
use crate::api::proto;
use crate::server::{ControlCommandError, control_ok};
use prost::Message;

pub(in crate::server) fn dispatch(
    state: &ServerState,
    session: crate::webrtc::SessionId,
    principal: &ApiPrincipal,
    classification: crate::access::ClientClassificationReason,
    command: proto::ExternalAuthenticationCommand,
) -> Result<control_ok::Result, ControlCommandError> {
    use proto::external_authentication_command::Action;
    let (action, target) = match command.action.as_ref() {
        Some(Action::ListIdentities(_)) => ("external_identity_list", None),
        Some(Action::ListSessions(_)) => ("browser_session_list", None),
        Some(Action::VerifyAdministratorBearer(_)) => ("administrator_bearer_verify", None),
        Some(Action::PrepareAdministratorVerification(_)) => {
            ("administrator_verification_prepare", None)
        }
        Some(Action::GetAdministratorVerification(_)) => ("administrator_verification_get", None),
        Some(Action::BeginRestoreVerification(_)) => ("restore_verification_begin", None),
        Some(Action::AppendRestoreVerification(_)) => ("restore_verification_append", None),
        Some(Action::GetRestoreVerification(_)) => ("restore_verification_get", None),
        Some(Action::ConfirmRestoreVerification(_)) => ("restore_verification_confirm", None),
        Some(Action::RevokeIdentity(request)) => (
            "external_identity_revoke",
            Uuid::parse_str(&request.identity_id)
                .ok()
                .map(|id| id.to_string()),
        ),
        Some(Action::RevokeSession(request)) => (
            "browser_session_revoke",
            Uuid::parse_str(&request.session_id)
                .ok()
                .map(|id| id.to_string()),
        ),
        None => ("external_administration", None),
    };
    let result = dispatch_inner(state, session, principal, command);
    super::super::record_access_audit(
        state,
        now_ms(),
        Some(&principal.id()),
        Some(principal.role),
        action,
        target.as_deref(),
        if result.is_ok() {
            "success"
        } else {
            "denied_or_invalid"
        },
        classification,
    );
    result
}

fn dispatch_inner(
    state: &ServerState,
    session: crate::webrtc::SessionId,
    principal: &ApiPrincipal,
    command: proto::ExternalAuthenticationCommand,
) -> Result<control_ok::Result, ControlCommandError> {
    if principal.role != AccessRole::Administrator {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            403,
            "Administrator access is required",
        ));
    }
    use proto::external_authentication_command::Action;
    use proto::external_authentication_result::Result as View;
    let result = match command.action {
        Some(Action::BeginRestoreVerification(request)) => View::RestoreVerification(
            verification::restore::begin(state, session, principal, request)?,
        ),
        Some(Action::AppendRestoreVerification(request)) => View::RestoreVerification(
            verification::restore::append(state, session, principal, request)?,
        ),
        Some(Action::GetRestoreVerification(request)) => View::RestoreVerification(
            verification::restore::get(state, session, principal, request)?,
        ),
        Some(Action::ConfirmRestoreVerification(request)) => View::RestoreVerification(
            verification::restore::confirm(state, session, principal, request)?,
        ),
        Some(Action::PrepareAdministratorVerification(request)) => View::AdministratorVerification(
            verification::browser::prepare(state, session, principal, request)?,
        ),
        Some(Action::GetAdministratorVerification(request)) => View::AdministratorVerification(
            verification::browser::get(state, session, principal, request)?,
        ),
        Some(Action::VerifyAdministratorBearer(request)) => View::AdministratorVerification(
            verification::verify_bearer(state, session, principal, request)?,
        ),
        Some(Action::ListIdentities(request)) => View::Identities(list_identities(state, request)?),
        Some(Action::ListSessions(request)) => View::Sessions(list_sessions(state, request)?),
        Some(Action::RevokeIdentity(request)) => {
            revoke_identity(state, request)?;
            View::Identities(list_identities(
                state,
                proto::ListExternalIdentities::default(),
            )?)
        }
        Some(Action::RevokeSession(request)) => {
            revoke_session(state, &request.session_id)?;
            View::Sessions(list_sessions(state, proto::ListBrowserSessions::default())?)
        }
        None => return Err(invalid("external authentication command has no action")),
    };
    Ok(control_ok::Result::ExternalAuthenticationResult(
        proto::ExternalAuthenticationResult {
            result: Some(result),
        },
    ))
}

fn revoke_identity(
    state: &ServerState,
    request: proto::RevokeExternalIdentity,
) -> Result<(), ControlCommandError> {
    let id = Uuid::parse_str(&request.identity_id)
        .map_err(|_| invalid("external identity ID is invalid"))?;
    let update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut registry = state
        .authentication
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let existing = registry
        .directory
        .records
        .iter()
        .find(|identity| identity.id == id)
        .ok_or_else(|| invalid("external identity was not found"))?;
    if request.expected_revision != existing.revision {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "external identity revision changed",
        ));
    }
    let administrator = existing.enabled && existing.role == AccessRole::Administrator;
    let mut candidate = registry.directory.clone();
    candidate
        .revoke(id)
        .map_err(|_| invalid("external identity cannot be revoked"))?;
    if administrator {
        require_retained_administrator(state, &candidate)?;
    }
    save_directory(state, &mut registry, candidate).map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "external identity could not be saved",
        )
    })?;
    let browsers: Vec<_> = registry
        .sessions
        .list()
        .filter(|browser| {
            browser
                .binding
                .is_some_and(|binding| binding.identity_id == id)
        })
        .map(|browser| browser.id)
        .collect();
    registry.sessions.revoke_identity(id);
    for browser in &browsers {
        registry.transactions.revoke_browser(*browser);
    }
    drop(registry);
    drop(update);
    for browser in browsers {
        close_browser_dependents(state, browser);
    }
    super::super::expire_api_sessions(state);
    Ok(())
}

fn require_retained_administrator(
    state: &ServerState,
    directory: &Directory,
) -> Result<(), ControlCommandError> {
    let inspect = || -> anyhow::Result<bool> {
        let path = state
            .camera_config_path
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("configuration storage unavailable"))?;
        let before = crate::config::load_config(path)?;
        let mut after = before.clone();
        after.source.insert(
            crate::access::identities::SECTION.into(),
            toml::Value::try_from(directory)?,
        );
        crate::access::migration::preserves_administrator(&before, &after, now_ms())
    };
    if !inspect().map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "Administrator paths could not be validated",
        )
    })? {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "replacement Administrator verification and confirmation are required",
        ));
    }
    Ok(())
}

fn revoke_session(state: &ServerState, id: &str) -> Result<(), ControlCommandError> {
    let id = Uuid::parse_str(id).map_err(|_| invalid("browser session ID is invalid"))?;
    {
        let mut registry = state
            .authentication
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        registry.sessions.revoke(id);
        registry.transactions.revoke_browser(id);
    }
    close_browser_dependents(state, id);
    super::super::expire_api_sessions(state);
    Ok(())
}

fn close_browser_dependents(state: &ServerState, id: Uuid) {
    let sessions: Vec<_> = state.api_session_owners.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter().filter_map(|(session_id, session)| {
            matches!(session.principal.identity, ApiPrincipalIdentity::External { browser, .. } if browser == id).then_some(*session_id)
        }).collect();
    for session in sessions {
        super::super::close_api_session(state, session);
        state.webrtc.request_api_session_close(session);
    }
}

fn invalid(message: &'static str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::InvalidRequest, 400, message)
}

fn page<T: Message>(
    mut rows: Vec<(Uuid, T)>,
    size: Option<u32>,
    token: &str,
) -> Result<(Vec<T>, String), ControlCommandError> {
    let size = size.unwrap_or(16);
    if !(1..=32).contains(&size) {
        return Err(invalid("page size must be between 1 and 32"));
    }
    let after = if token.is_empty() {
        None
    } else {
        Some(Uuid::parse_str(token).map_err(|_| invalid("page token is invalid"))?)
    };
    rows.sort_unstable_by_key(|(id, _)| *id);
    let mut selected = Vec::new();
    let mut bytes = 0;
    let mut last = Uuid::nil();
    for (id, row) in rows
        .into_iter()
        .filter(|(id, _)| after.is_none_or(|after| *id > after))
    {
        let row_bytes = row.encoded_len() + 16;
        if row_bytes > 48 * 1_024 {
            return Err(invalid("identity exceeds the control-message limit"));
        }
        if selected.len() == usize::try_from(size).expect("bounded page size")
            || bytes + row_bytes > 48 * 1_024
        {
            return Ok((selected, last.to_string()));
        }
        bytes += row_bytes;
        last = id;
        selected.push(row);
    }
    Ok((selected, String::new()))
}

fn list_identities(
    state: &ServerState,
    request: proto::ListExternalIdentities,
) -> Result<proto::ExternalIdentityList, ControlCommandError> {
    let registry = state
        .authentication
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let rows = registry
        .directory
        .records
        .iter()
        .map(|identity| {
            let provider = registry.config.as_ref().and_then(|config| {
                config
                    .providers
                    .iter()
                    .find(|provider| provider.id == identity.provider_id)
            });
            (
                identity.id,
                proto::ExternalIdentity {
                    identity_id: identity.id.to_string(),
                    provider_id: identity.provider_id.clone(),
                    provider_name: provider
                        .map_or_else(String::new, |provider| provider.name.clone()),
                    subject_fingerprint: identity.subject_fingerprint.clone(),
                    display_name: identity.display_name.clone(),
                    role: super::super::proto_access_role(identity.role),
                    camera_access: Some(proto::CameraAccessPolicy {
                        all_cameras: identity.camera_access.all_cameras,
                        camera_ids: identity.camera_access.camera_ids.clone(),
                        group_ids: identity.camera_access.group_ids.clone(),
                    }),
                    revision: identity.revision,
                    enabled: identity.enabled,
                    created_at_ms: identity.created_at_ms,
                },
            )
        })
        .collect();
    let (identities, next_page_token) = page(rows, request.page_size, &request.page_token)?;
    Ok(proto::ExternalIdentityList {
        identities,
        next_page_token,
    })
}

fn list_sessions(
    state: &ServerState,
    request: proto::ListBrowserSessions,
) -> Result<proto::BrowserSessionList, ControlCommandError> {
    let mut registry = state
        .authentication
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.sessions.expire(Instant::now());
    let rows = registry
        .sessions
        .list()
        .filter_map(|browser| {
            let binding = browser.binding?;
            registry
                .directory
                .active(binding.identity_id, binding.revision)?;
            Some((
                browser.id,
                proto::BrowserSession {
                    session_id: browser.id.to_string(),
                    identity_id: binding.identity_id.to_string(),
                    created_at_ms: browser.created_at_ms,
                    last_activity_at_ms: browser.last_activity_at_ms(),
                    absolute_expires_at_ms: browser.absolute_expires_at_ms,
                },
            ))
        })
        .collect();
    let (sessions, next_page_token) = page(rows, request.page_size, &request.page_token)?;
    Ok(proto::BrowserSessionList {
        sessions,
        next_page_token,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn persisted_browser_state() -> (ServerState, String, std::path::PathBuf) {
        let (mut state, handle, _) = super::super::tests::browser_state();
        let directory =
            std::env::temp_dir().join(format!("keeppeek-identity-revoke-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("config.toml");
        let registry = state.authentication.lock().unwrap();
        let mut root = toml::Table::new();
        root.insert(
            "external_auth".into(),
            toml::Value::try_from(registry.config.as_ref().unwrap()).unwrap(),
        );
        root.insert(
            "external_identities".into(),
            toml::Value::try_from(&registry.directory).unwrap(),
        );
        crate::config::write_configuration_table(&path, &root).unwrap();
        drop(registry);
        state.camera_config_path = Some(path);
        (state, handle, directory)
    }

    #[test]
    fn identity_revocation_is_revision_checked_persistent_and_invalidates_browsers() {
        let (state, handle, directory) = persisted_browser_state();
        let identity = state.authentication.lock().unwrap().directory.records[0].clone();
        let request = proto::RevokeExternalIdentity {
            identity_id: identity.id.to_string(),
            expected_revision: identity.revision + 1,
        };
        assert!(revoke_identity(&state, request).is_err());
        let browser_request = super::super::tests::remote_request("GET", &handle, None);
        assert!(super::super::super::api_principal(&browser_request, &state).is_ok());
        revoke_identity(
            &state,
            proto::RevokeExternalIdentity {
                identity_id: identity.id.to_string(),
                expected_revision: identity.revision,
            },
        )
        .unwrap();
        assert!(super::super::super::api_principal(&browser_request, &state).is_err());
        let root = crate::config::load_configuration_table(&directory.join("config.toml")).unwrap();
        assert!(!Directory::from_root(&root).unwrap().records[0].enabled);
        assert!(
            state
                .authentication
                .lock()
                .unwrap()
                .sessions
                .list()
                .all(|browser| browser.binding.is_none())
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn last_administrator_rejection_preserves_file_identity_and_browser() {
        let (state, handle, directory) = persisted_browser_state();
        let path = directory.join("config.toml");
        let mut registry = state.authentication.lock().unwrap();
        let identity = &mut registry.directory.records[0];
        identity.role = AccessRole::Administrator;
        identity.camera_access = crate::access::CameraAccess::unrestricted();
        let identity = identity.clone();
        let rule = &mut registry.config.as_mut().unwrap().providers[0].mappings[0];
        rule.role = AccessRole::Administrator;
        rule.camera_access = None;
        let mut root = crate::config::load_configuration_table(&path).unwrap();
        root.insert(
            "external_auth".into(),
            toml::Value::try_from(registry.config.as_ref().unwrap()).unwrap(),
        );
        root.insert(
            "external_identities".into(),
            toml::Value::try_from(&registry.directory).unwrap(),
        );
        crate::config::write_configuration_table(&path, &root).unwrap();
        drop(registry);
        let before = std::fs::read(&path).unwrap();
        let error = revoke_identity(
            &state,
            proto::RevokeExternalIdentity {
                identity_id: identity.id.to_string(),
                expected_revision: identity.revision,
            },
        )
        .unwrap_err();
        assert_eq!(error.code, proto::ErrorCode::Rejected);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let request = super::super::tests::remote_request("GET", &handle, None);
        assert_eq!(
            super::super::super::api_principal(&request, &state)
                .unwrap()
                .principal
                .role,
            AccessRole::Administrator
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn failed_identity_persistence_does_not_revoke_live_authorization() {
        let (mut state, handle, directory) = persisted_browser_state();
        let identity = state.authentication.lock().unwrap().directory.records[0].clone();
        let before = std::fs::read(directory.join("config.toml")).unwrap();
        state.camera_config_path = Some(directory.join("missing").join("config.toml"));
        let result = revoke_identity(
            &state,
            proto::RevokeExternalIdentity {
                identity_id: identity.id.to_string(),
                expected_revision: identity.revision,
            },
        );
        assert!(result.is_err());
        assert_eq!(
            std::fs::read(directory.join("config.toml")).unwrap(),
            before
        );
        let request = super::super::tests::remote_request("GET", &handle, None);
        assert!(super::super::super::api_principal(&request, &state).is_ok());
        assert!(
            state
                .authentication
                .lock()
                .unwrap()
                .directory
                .active(identity.id, identity.revision)
                .is_some()
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn browser_revocation_invalidates_authorization_and_is_administrator_only() {
        let (state, handle, _) = super::super::tests::browser_state();
        let request = super::super::tests::remote_request("GET", &handle, None);
        let principal = super::super::super::api_principal(&request, &state)
            .unwrap()
            .principal;
        let sessions = list_sessions(&state, proto::ListBrowserSessions::default()).unwrap();
        let command = proto::ExternalAuthenticationCommand {
            action: Some(
                proto::external_authentication_command::Action::RevokeSession(
                    proto::RevokeBrowserSession {
                        session_id: sessions.sessions[0].session_id.clone(),
                    },
                ),
            ),
        };
        let classification = crate::access::ClientClassificationReason::DirectLocal;
        assert!(
            dispatch(
                &state,
                crate::webrtc::SessionId::from_u64(0),
                &principal,
                classification,
                command.clone()
            )
            .is_err()
        );
        assert!(active(&state, &principal, Instant::now(), now_ms()));
        let administrator = ApiPrincipal::local("127.0.0.1".parse().unwrap());
        dispatch(
            &state,
            crate::webrtc::SessionId::from_u64(0),
            &administrator,
            classification,
            command.clone(),
        )
        .unwrap();
        assert!(!active(&state, &principal, Instant::now(), now_ms()));
        assert!(super::super::super::api_principal(&request, &state).is_err());
        dispatch(
            &state,
            crate::webrtc::SessionId::from_u64(0),
            &administrator,
            classification,
            command,
        )
        .unwrap();
        assert!(
            list_sessions(&state, proto::ListBrowserSessions::default())
                .unwrap()
                .sessions
                .is_empty()
        );
    }

    #[test]
    fn directory_pages_are_bounded_and_do_not_expose_browser_secrets() {
        let (state, handle, csrf) = super::super::tests::browser_state();
        let request = proto::ListExternalIdentities {
            page_size: Some(1),
            page_token: String::new(),
        };
        let result = list_identities(&state, request).unwrap();
        assert_eq!(result.identities.len(), 1);
        assert_eq!(result.identities[0].display_name, "Alice");
        assert!(result.next_page_token.is_empty());
        let sessions = list_sessions(&state, proto::ListBrowserSessions::default()).unwrap();
        assert_eq!(sessions.sessions.len(), 1);
        let diagnostic = format!("{result:?} {sessions:?}");
        assert!(!diagnostic.contains(&handle));
        assert!(!diagnostic.contains(&csrf));
        assert!(
            list_identities(
                &state,
                proto::ListExternalIdentities {
                    page_size: Some(0),
                    page_token: String::new()
                }
            )
            .is_err()
        );
        assert!(
            list_sessions(
                &state,
                proto::ListBrowserSessions {
                    page_size: Some(33),
                    page_token: String::new()
                }
            )
            .is_err()
        );
    }
}
