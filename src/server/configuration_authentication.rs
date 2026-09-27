//! Plans global authentication changes through the existing configuration revision owner.

use super::*;

pub(super) fn plan(
    state: &ServerState,
    root: toml::Table,
    revision: String,
    update: proto::ExternalAuthenticationUpdate,
) -> Result<proto::ConfigurationPlan, ControlCommandError> {
    let path = state
        .camera_config_path
        .as_deref()
        .ok_or_else(|| rejected("configuration storage is unavailable"))?;
    let before = config::validated_configuration_table(path, &root)
        .map_err(|_| rejected("current authentication settings could not be validated"))?;
    let mut candidate = root;
    apply_update(&mut candidate, update)?;
    let after = config::validated_configuration_table(path, &candidate)
        .map_err(|_| rejected("candidate authentication settings could not be validated"))?;
    if before.external_auth == after.external_auth {
        return Err(rejected("authentication settings did not change"));
    }
    let requires_confirmation = needs_confirmation(&before, &after)?;
    let plan = proto::ConfigurationPlan {
        plan_id: Uuid::new_v4().simple().to_string(), configuration_revision: revision,
        expires_at_ms: i64::try_from(unix_time_ms()).unwrap_or(i64::MAX).saturating_add(300_000),
        authoritative_target_count: 0, targets: Vec::new(), issues: Vec::new(),
        changes: vec![proto::ConfigurationFieldChange {
            field: "external_auth".into(), old_configured_value: summary(&before),
            new_configured_value: summary(&after), secret: true,
            source: proto::ConfigurationValueSource::Override as i32, ..Default::default()
        }],
        impact: proto::ConfigurationImpact::Immediate as i32, valid: true,
        apply_semantics: "Authentication changes invalidate browser sessions and pending sign-ins after atomic persistence. Camera configuration is unchanged.".into(),
        requires_administrator_confirmation: requires_confirmation,
    };
    state.configuration_plans.insert(StoredPlan {
        plan: plan.clone(),
        candidate,
        target_ids: Vec::new(),
    });
    Ok(plan)
}

fn apply_update(
    root: &mut toml::Table,
    update: proto::ExternalAuthenticationUpdate,
) -> Result<(), ControlCommandError> {
    match update.value {
        Some(proto::external_authentication_update::Value::Set(settings)) => {
            let settings = crate::access::external_proto::decode(settings)
                .map_err(|_| rejected("external authentication settings are invalid"))?;
            root.insert(
                "external_auth".into(),
                toml::Value::try_from(settings).map_err(|_| {
                    rejected("external authentication settings could not be encoded")
                })?,
            );
        }
        Some(proto::external_authentication_update::Value::Clear(true)) => {
            root.remove("external_auth");
        }
        _ => {
            return Err(rejected(
                "set authentication settings or explicitly clear them",
            ));
        }
    }
    Ok(())
}

fn summary(config: &Config) -> String {
    config.external_auth.as_ref().map_or_else(
        || "External authentication disabled".into(),
        |settings| {
            format!(
                "{} configured provider(s); bearer transition {}",
                settings.providers.len(),
                if settings.bearer_enabled {
                    "enabled"
                } else {
                    "disabled"
                }
            )
        },
    )
}

fn rejected(message: &'static str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::Rejected, 409, message)
}

fn needs_confirmation(before: &Config, after: &Config) -> Result<bool, ControlCommandError> {
    let now = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    let inspect = || -> anyhow::Result<bool> {
        Ok(
            crate::access::migration::has_current_administrator(before, now)?
                && !crate::access::migration::preserves_administrator(before, after, now)?,
        )
    };
    inspect().map_err(|_| rejected("Administrator paths could not be validated"))
}

pub(super) fn prepare_activation(
    state: &ServerState,
    before: &Config,
    after: &Config,
) -> Result<Option<authentication::Registry>, ControlCommandError> {
    if before.external_auth == after.external_auth {
        return Ok(None);
    }
    if needs_confirmation(before, after)? {
        return Err(rejected(
            "replacement Administrator verification and confirmation are required",
        ));
    }
    authentication::prepare_configuration(state, before, after)
        .map(Some)
        .map_err(|_| rejected("authentication configuration changed; reload before applying"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_authentication_plan_is_global_validated_and_does_not_write() {
        let directory = std::env::temp_dir().join(format!("keeppeek-auth-plan-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("config.toml");
        std::fs::write(&path, "host = '127.0.0.1'\noperator_note = 'preserve'\n").unwrap();
        let state = ServerState::empty().with_camera_config_path(path.clone());
        let revision = current_configuration_revision(&state).unwrap();
        let settings = proxy_settings();
        let before = std::fs::read(&path).unwrap();
        let plan = plan_configuration_change(
            &state,
            proto::PlanConfigurationChange {
                expected_configuration_revision: revision.clone(),
                targets: None,
                change: Some(proto::ConfigurationChange {
                    change: Some(proto::configuration_change::Change::ExternalAuthentication(
                        proto::ExternalAuthenticationUpdate {
                            value: Some(proto::external_authentication_update::Value::Set(
                                settings,
                            )),
                        },
                    )),
                }),
            },
        )
        .unwrap();
        assert!(plan.valid);
        assert_eq!(plan.authoritative_target_count, 0);
        assert!(plan.targets.is_empty());
        assert_eq!(plan.configuration_revision, revision);
        assert_eq!(plan.changes[0].field, "external_auth");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(
            state
                .configuration_plans
                .get(&plan.plan_id)
                .unwrap()
                .candidate["operator_note"]
                .as_str(),
            Some("preserve")
        );
        let applied = apply_configuration_plan(
            &state,
            proto::ApplyConfigurationPlan {
                plan_id: plan.plan_id,
                expected_configuration_revision: revision,
                administrator_confirmation: None,
            },
        )
        .unwrap();
        assert!(applied.configuration_committed);
        let bootstrap = rouille::Request::fake_https_from(
            "198.51.100.1:1234".parse().unwrap(),
            "GET",
            "/auth/session",
            vec![("Host".into(), "keeppeek.example".into())],
            vec![],
        );
        let response = authentication::handle(&bootstrap, &state).unwrap();
        assert_eq!(response.status_code, 200);
        let body: serde_json::Value =
            serde_json::from_reader(response.data.into_reader_and_size().0).unwrap();
        assert_eq!(body["methods"][0]["id"], "company");
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn proxy_settings() -> proto::ExternalAuthenticationSettings {
        proto::ExternalAuthenticationSettings {
            allowed_origins: vec!["https://keeppeek.example".into()],
            providers: vec![proto::ExternalAuthenticationProvider {
                provider_id: "company".into(),
                name: "Company".into(),
                mappings: vec![proto::ExternalRoleMapping {
                    claim: "role".into(),
                    value: "admins".into(),
                    role: proto::AccessRole::Administrator as i32,
                    camera_access: None,
                }],
                method: Some(proto::external_authentication_provider::Method::Proxy(
                    proto::ProxyAuthenticationSettings {
                        trusted_peers: vec!["203.0.113.1/32".into()],
                        subject_header: "X-Identity-Subject".into(),
                        role_header: "X-Identity-Role".into(),
                        ..Default::default()
                    },
                )),
            }],
            ..Default::default()
        }
    }
}
