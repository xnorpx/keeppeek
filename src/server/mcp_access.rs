//! Administrator-only WebRTC control handler for MCP access lifecycle governance.
//!
//! Enforces the asymmetric human-in-the-loop contract:
//! - Any Administrator (human browser session, trusted local, or Bearer) can inspect state or
//!   disable MCP access live without restarting KeepPeek.
//! - Enabling MCP access or silencing the 7-day long-enabled warning strictly requires a human
//!   Administrator session (trusted local network or interactive browser session) and rejects
//!   Bearer-authenticated principals with `403 Rejected`.

use super::{ApiPrincipal, ControlCommandError, ServerState, configuration_revision, unix_time_ms};
use crate::{
    access::AccessRole,
    api::proto::{self, mcp_access_command, mcp_access_result, ok as control_ok},
    config::{
        self,
        mcp::{MCP_LONG_ENABLED_WARNING_AFTER_SECS, McpConfig, UpdateError},
    },
};
use std::path::Path;

pub(super) const CAPABILITY_ID: &str = "keeppeek.mcp-access.v1";

pub(super) const fn audit_operation(command: &proto::McpAccessCommand) -> Option<&'static str> {
    match command.action {
        Some(mcp_access_command::Action::SetEnabled(_)) => Some("mcp_access_update"),
        Some(mcp_access_command::Action::SilenceLongEnabledWarning(_)) => {
            Some("mcp_warning_silence")
        }
        Some(mcp_access_command::Action::Get(_)) | None => None,
    }
}

pub(super) fn dispatch(
    state: &ServerState,
    principal: &ApiPrincipal,
    command: proto::McpAccessCommand,
) -> Result<control_ok::Result, ControlCommandError> {
    if principal.role != AccessRole::Administrator {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            403,
            "administrator access is required for MCP access controls",
        ));
    }
    let result = match command.action {
        Some(mcp_access_command::Action::Get(_)) => {
            mcp_access_result::Result::State(get_state(state, principal)?)
        }
        Some(mcp_access_command::Action::SetEnabled(request)) => {
            mcp_access_result::Result::State(set_enabled(state, principal, &request)?)
        }
        Some(mcp_access_command::Action::SilenceLongEnabledWarning(request)) => {
            mcp_access_result::Result::State(silence_warning(state, principal, &request)?)
        }
        None => {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "MCP access command has no action",
            ));
        }
    };
    Ok(control_ok::Result::McpAccessResult(
        proto::McpAccessResult {
            result: Some(result),
        },
    ))
}

const fn human_control_allowed(principal: &ApiPrincipal) -> bool {
    principal.credential_binding().is_none()
}

fn require_config_path(state: &ServerState) -> Result<&Path, ControlCommandError> {
    state.camera_config_path.as_deref().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "MCP access controls require an application configuration file",
        )
    })
}

fn now_unix_secs() -> u64 {
    (unix_time_ms() / 1_000).max(1)
}

const fn proto_state(
    configuration_revision: String,
    config: McpConfig,
    principal: &ApiPrincipal,
    now_secs: u64,
) -> proto::McpAccessState {
    proto::McpAccessState {
        configuration_revision,
        enabled: config.enabled,
        enabled_at_unix_secs: config.enabled_at_unix_secs,
        enabled_duration_secs: config.enabled_duration_secs(now_secs),
        long_enabled_warning_threshold_secs: MCP_LONG_ENABLED_WARNING_AFTER_SECS,
        long_enabled_warning_active: config.long_enabled_warning_active(now_secs),
        long_enabled_warning_silenced: config.long_enabled_warning_silenced,
        allow_writes: config.allow_writes,
        allow_camera_device_writes: config.allow_camera_device_writes,
        allow_snapshots: config.allow_snapshots,
        human_control_allowed: human_control_allowed(principal),
    }
}

fn get_state(
    state: &ServerState,
    principal: &ApiPrincipal,
) -> Result<proto::McpAccessState, ControlCommandError> {
    let config_path = require_config_path(state)?;
    let _lock = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let loaded = config::load_config(config_path).map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "unable to load MCP settings",
        )
    })?;
    let revision = configuration_revision(&loaded);
    Ok(proto_state(
        revision,
        loaded.mcp,
        principal,
        now_unix_secs(),
    ))
}

fn set_enabled(
    state: &ServerState,
    principal: &ApiPrincipal,
    request: &proto::SetMcpAccess,
) -> Result<proto::McpAccessState, ControlCommandError> {
    if request.enabled {
        if !human_control_allowed(principal) {
            return Err(ControlCommandError::new(
                proto::ErrorCode::Rejected,
                403,
                "enabling MCP access requires trusted local access or an interactive OIDC/identity-proxy browser session, not a Bearer credential",
            ));
        }
        if !request.confirm {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "enabling MCP access requires explicit confirmation (confirm = true)",
            ));
        }
    }
    let config_path = require_config_path(state)?;
    let _lock = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let current = config::load_config(config_path).map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "unable to load MCP settings",
        )
    })?;
    let current_revision = configuration_revision(&current);
    if request.expected_configuration_revision.is_empty()
        || current_revision != request.expected_configuration_revision
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "configuration revision conflicts; reload settings and retry",
        ));
    }
    let now_secs = now_unix_secs();
    let (updated, next_revision) = match config::mcp::update(
        config_path,
        Some(&request.expected_configuration_revision),
        |mcp| {
            mcp.apply_enabled_transition(request.enabled, now_secs);
            Ok(())
        },
    ) {
        Ok(result) => result,
        Err(error) => {
            if !request.enabled && error != UpdateError::RevisionConflict {
                let mut live_disabled = state.mcp.config();
                live_disabled.apply_enabled_transition(false, now_secs);
                state.mcp.apply_live_config(live_disabled);
                return Err(ControlCommandError::new(
                    proto::ErrorCode::Internal,
                    500,
                    "MCP access was disabled live, but config.toml could not be saved",
                ));
            }
            return Err(map_update_error(error));
        }
    };
    state.mcp.apply_live_config(updated.mcp);
    tracing::info!(
        enabled = updated.mcp.enabled,
        principal_id = %principal.id(),
        "updated MCP access state live"
    );
    Ok(proto_state(next_revision, updated.mcp, principal, now_secs))
}

fn silence_warning(
    state: &ServerState,
    principal: &ApiPrincipal,
    request: &proto::SilenceMcpLongEnabledWarning,
) -> Result<proto::McpAccessState, ControlCommandError> {
    if !human_control_allowed(principal) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            403,
            "silencing the MCP long-enabled warning requires trusted local access or an interactive OIDC/identity-proxy browser session, not a Bearer credential",
        ));
    }
    let config_path = require_config_path(state)?;
    let _lock = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let current = config::load_config(config_path).map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "unable to load MCP settings",
        )
    })?;
    let current_revision = configuration_revision(&current);
    if request.expected_configuration_revision.is_empty()
        || current_revision != request.expected_configuration_revision
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "configuration revision conflicts; reload settings and retry",
        ));
    }
    let (updated, next_revision) = config::mcp::update(
        config_path,
        Some(&request.expected_configuration_revision),
        |mcp| {
            if mcp.silence_long_enabled_warning() {
                Ok(())
            } else {
                Err(UpdateError::InvalidTransition)
            }
        },
    )
    .map_err(map_update_error)?;
    state.mcp.apply_live_config(updated.mcp);
    tracing::info!(
        principal_id = %principal.id(),
        "silenced MCP long-enabled warning until MCP is disabled"
    );
    Ok(proto_state(
        next_revision,
        updated.mcp,
        principal,
        now_unix_secs(),
    ))
}

fn map_update_error(error: UpdateError) -> ControlCommandError {
    match error {
        UpdateError::RevisionConflict => ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "configuration revision conflicts; reload settings and retry",
        ),
        UpdateError::InvalidTransition => ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "MCP must be enabled before its long-enabled warning can be silenced",
        ),
        UpdateError::Validation => ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "MCP settings could not be validated",
        ),
        UpdateError::Persistence => ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "unable to save MCP settings",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::ApiPrincipalIdentity;
    use std::net::{IpAddr, Ipv4Addr};
    use uuid::Uuid;

    fn write_test_config(body: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("keeppeek-mcp-access-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(not(windows))]
        let dir = dir.canonicalize().unwrap();
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, body).unwrap();
        std::fs::write(config::secrets_path(&config_path), "").unwrap();
        (dir, config_path)
    }

    fn bearer_admin() -> ApiPrincipal {
        ApiPrincipal {
            identity: ApiPrincipalIdentity::Credential {
                id: Uuid::new_v4(),
                revision: 1,
            },
            display_name: "Agent Bearer".to_owned(),
            role: AccessRole::Administrator,
            credential_expires_at_ms: None,
        }
    }

    fn browser_admin() -> ApiPrincipal {
        ApiPrincipal {
            identity: ApiPrincipalIdentity::External {
                id: Uuid::new_v4(),
                revision: 1,
                browser: Uuid::new_v4(),
            },
            display_name: "Human Admin".to_owned(),
            role: AccessRole::Administrator,
            credential_expires_at_ms: None,
        }
    }

    fn local_admin() -> ApiPrincipal {
        ApiPrincipal::local(IpAddr::V4(Ipv4Addr::LOCALHOST))
    }

    fn user_principal() -> ApiPrincipal {
        ApiPrincipal {
            identity: ApiPrincipalIdentity::External {
                id: Uuid::new_v4(),
                revision: 1,
                browser: Uuid::new_v4(),
            },
            display_name: "Viewer".to_owned(),
            role: AccessRole::User,
            credential_expires_at_ms: None,
        }
    }

    fn extract_state(result: control_ok::Result) -> proto::McpAccessState {
        let control_ok::Result::McpAccessResult(proto::McpAccessResult {
            result: Some(mcp_access_result::Result::State(state)),
        }) = result
        else {
            panic!("expected McpAccessState result");
        };
        state
    }

    #[test]
    fn enforces_human_only_enable_and_silence_while_allowing_admin_disable() {
        let (dir, config_path) = write_test_config("host = \"127.0.0.1\"\nport = 8081\n");
        let mut state = ServerState::empty();
        state.camera_config_path = Some(config_path);

        // Non-admin User is rejected on all MCP access commands.
        let user_err = dispatch(
            &state,
            &user_principal(),
            proto::McpAccessCommand {
                action: Some(mcp_access_command::Action::Get(proto::GetMcpAccess {})),
            },
        )
        .unwrap_err();
        assert_eq!(user_err._http_status, 403);

        // Bearer admin can read state (`human_control_allowed == false`), cannot enable or silence.
        let bearer = bearer_admin();
        let initial = extract_state(
            dispatch(
                &state,
                &bearer,
                proto::McpAccessCommand {
                    action: Some(mcp_access_command::Action::Get(proto::GetMcpAccess {})),
                },
            )
            .unwrap(),
        );
        assert!(!initial.enabled);
        assert!(!initial.human_control_allowed);

        let enable_via_bearer = dispatch(
            &state,
            &bearer,
            proto::McpAccessCommand {
                action: Some(mcp_access_command::Action::SetEnabled(
                    proto::SetMcpAccess {
                        expected_configuration_revision: initial.configuration_revision.clone(),
                        enabled: true,
                        confirm: true,
                    },
                )),
            },
        )
        .unwrap_err();
        assert_eq!(enable_via_bearer._http_status, 403);
        assert!(!state.mcp_enabled());

        // Human browser admin requires confirm = true to enable.
        let human = browser_admin();
        let unconfirmed = dispatch(
            &state,
            &human,
            proto::McpAccessCommand {
                action: Some(mcp_access_command::Action::SetEnabled(
                    proto::SetMcpAccess {
                        expected_configuration_revision: initial.configuration_revision.clone(),
                        enabled: true,
                        confirm: false,
                    },
                )),
            },
        )
        .unwrap_err();
        assert_eq!(unconfirmed._http_status, 400);

        // Human browser admin enables MCP live with confirm = true.
        let enabled = extract_state(
            dispatch(
                &state,
                &human,
                proto::McpAccessCommand {
                    action: Some(mcp_access_command::Action::SetEnabled(
                        proto::SetMcpAccess {
                            expected_configuration_revision: initial.configuration_revision.clone(),
                            enabled: true,
                            confirm: true,
                        },
                    )),
                },
            )
            .unwrap(),
        );
        assert!(enabled.enabled);
        assert!(enabled.human_control_allowed);
        assert!(enabled.enabled_at_unix_secs.is_some());
        assert!(!enabled.long_enabled_warning_silenced);
        assert!(state.mcp_enabled());

        // Stale revision is rejected with 409.
        let stale = dispatch(
            &state,
            &human,
            proto::McpAccessCommand {
                action: Some(mcp_access_command::Action::SilenceLongEnabledWarning(
                    proto::SilenceMcpLongEnabledWarning {
                        expected_configuration_revision: initial.configuration_revision,
                    },
                )),
            },
        )
        .unwrap_err();
        assert_eq!(stale._http_status, 409);

        // Bearer cannot silence the warning.
        let bearer_silence = dispatch(
            &state,
            &bearer,
            proto::McpAccessCommand {
                action: Some(mcp_access_command::Action::SilenceLongEnabledWarning(
                    proto::SilenceMcpLongEnabledWarning {
                        expected_configuration_revision: enabled.configuration_revision.clone(),
                    },
                )),
            },
        )
        .unwrap_err();
        assert_eq!(bearer_silence._http_status, 403);

        // Human local admin can silence the warning.
        let silenced = extract_state(
            dispatch(
                &state,
                &local_admin(),
                proto::McpAccessCommand {
                    action: Some(mcp_access_command::Action::SilenceLongEnabledWarning(
                        proto::SilenceMcpLongEnabledWarning {
                            expected_configuration_revision: enabled.configuration_revision,
                        },
                    )),
                },
            )
            .unwrap(),
        );
        assert!(silenced.long_enabled_warning_silenced);
        assert!(!silenced.long_enabled_warning_active);

        // Even Bearer admin can disable MCP live, clearing timestamp and silence.
        let disabled = extract_state(
            dispatch(
                &state,
                &bearer,
                proto::McpAccessCommand {
                    action: Some(mcp_access_command::Action::SetEnabled(
                        proto::SetMcpAccess {
                            expected_configuration_revision: silenced.configuration_revision,
                            enabled: false,
                            confirm: false,
                        },
                    )),
                },
            )
            .unwrap(),
        );
        assert!(!disabled.enabled);
        assert!(disabled.enabled_at_unix_secs.is_none());
        assert!(!disabled.long_enabled_warning_silenced);
        assert!(!state.mcp_enabled());

        // Silencing while disabled fails with 409 so callers cannot pre-silence a future enable.
        let silence_disabled = dispatch(
            &state,
            &human,
            proto::McpAccessCommand {
                action: Some(mcp_access_command::Action::SilenceLongEnabledWarning(
                    proto::SilenceMcpLongEnabledWarning {
                        expected_configuration_revision: disabled.configuration_revision,
                    },
                )),
            },
        )
        .unwrap_err();
        assert_eq!(silence_disabled._http_status, 409);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
