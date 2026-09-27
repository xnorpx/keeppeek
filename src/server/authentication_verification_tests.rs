use super::{Proof, Registry, Stage, commit, verify_bearer};
use crate::access::{
    AccessKey, AccessManager, AccessRole, CameraAccess, ClientClassification,
    ClientClassificationReason,
};
use crate::api::proto;
use crate::server::{ApiPrincipal, ApiSessionRecord, ServerState, configuration};
use std::{path::PathBuf, time::Instant};

pub(super) const SESSION: crate::webrtc::SessionId = crate::webrtc::SessionId::from_u64(125);

pub(super) struct Fixture {
    pub(super) state: ServerState,
    pub(super) owner: ApiPrincipal,
    pub(super) key: AccessKey,
    pub(super) directory: PathBuf,
    pub(super) plan: proto::ConfigurationPlan,
    pub(super) cookie: String,
}

impl Fixture {
    pub(super) fn new() -> Self {
        let (mut state, cookie, _) = super::super::tests::browser_state();
        let directory =
            std::env::temp_dir().join(format!("keeppeek-verification-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("config.toml");
        state.camera_config_path = Some(path.clone());
        let owner = {
            let mut registry = state.authentication.lock().unwrap();
            let settings = registry.config.as_mut().unwrap();
            settings.providers[0].mappings[0].role = AccessRole::Administrator;
            settings.providers[0].mappings[0].camera_access = None;
            let policy =
                crate::access::external::admission_policy_fingerprint(&settings.providers[0])
                    .unwrap();
            let identity = &mut registry.directory.records[0];
            identity.role = AccessRole::Administrator;
            identity.camera_access = CameraAccess::unrestricted();
            identity.admission_policy_fingerprint = Some(policy);
            let identity = identity.clone();
            let browser = registry
                .sessions
                .lookup(&cookie, "https://keeppeek.example", Instant::now())
                .unwrap();
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
            super::super::principal(&identity, browser.id, i64::MAX)
        };
        state.access_manager = AccessManager::open_with_config_update(
            &path,
            AccessKey::unset(),
            state.config_update.clone(),
        )
        .unwrap();
        let key = state
            .access_manager
            .create_credential("Replacement", None, AccessRole::Administrator, None, 1)
            .unwrap()
            .access_key;
        bind_session(&state, SESSION, &owner);
        let plan = clear_plan(&state, &owner);
        assert!(plan.requires_administrator_confirmation);
        Self {
            state,
            owner,
            key,
            directory,
            plan,
            cookie,
        }
    }

    fn verify(&self) -> proto::AdministratorVerification {
        verify_bearer(
            &self.state,
            SESSION,
            &self.owner,
            proto::VerifyAdministratorBearer {
                configuration_plan_id: self.plan.plan_id.clone(),
                access_key: self.key.canonical(),
            },
        )
        .unwrap()
    }

    pub(super) fn apply(
        &self,
        session: crate::webrtc::SessionId,
        confirmation: Option<proto::AdministratorConfirmation>,
    ) -> Result<crate::api::proto::ok::Result, crate::server::ControlCommandError> {
        configuration::dispatch_as(
            &self.state,
            session,
            &self.owner,
            proto::ConfigurationCommand {
                action: Some(proto::configuration_command::Action::Apply(
                    proto::ApplyConfigurationPlan {
                        plan_id: self.plan.plan_id.clone(),
                        expected_configuration_revision: self.plan.configuration_revision.clone(),
                        administrator_confirmation: confirmation,
                    },
                )),
            },
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.state
            .configuration_plans
            .restore_proofs
            .lock()
            .unwrap()
            .close_session(SESSION);
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}

fn bind_session(state: &ServerState, id: crate::webrtc::SessionId, principal: &ApiPrincipal) {
    let address = "203.0.113.1".parse().unwrap();
    state.api_session_owners.lock().unwrap().insert(
        id,
        ApiSessionRecord {
            lifecycle: Default::default(),
            principal: principal.clone(),
            classification: ClientClassification {
                peer_address: address,
                effective_address: address,
                local: false,
                reason: ClientClassificationReason::DirectRemote,
            },
            created_at_ms: 1,
            last_activity_at_ms: 1,
            absolute_expires_at_ms: i64::MAX,
            last_activity: Instant::now(),
        },
    );
}

fn clear_plan(state: &ServerState, principal: &ApiPrincipal) -> proto::ConfigurationPlan {
    let response = configuration::dispatch_as(
        state,
        SESSION,
        principal,
        proto::ConfigurationCommand {
            action: Some(
                proto::configuration_command::Action::GetExternalAuthentication(
                    proto::GetExternalAuthenticationConfiguration {},
                ),
            ),
        },
    )
    .unwrap();
    let proto::ok::Result::ConfigurationResult(response) = response else {
        panic!("wrong result")
    };
    let Some(proto::configuration_result::Result::ExternalAuthentication(settings)) =
        response.result
    else {
        panic!("wrong settings")
    };
    let response = configuration::dispatch_as(
        state,
        SESSION,
        principal,
        proto::ConfigurationCommand {
            action: Some(proto::configuration_command::Action::Plan(
                proto::PlanConfigurationChange {
                    expected_configuration_revision: settings.configuration_revision,
                    targets: None,
                    change: Some(proto::ConfigurationChange {
                        change: Some(proto::configuration_change::Change::ExternalAuthentication(
                            proto::ExternalAuthenticationUpdate {
                                value: Some(proto::external_authentication_update::Value::Clear(
                                    true,
                                )),
                            },
                        )),
                    }),
                },
            )),
        },
    )
    .unwrap();
    let proto::ok::Result::ConfigurationResult(response) = response else {
        panic!("wrong result")
    };
    let Some(proto::configuration_result::Result::Plan(plan)) = response.result else {
        panic!("wrong plan")
    };
    plan
}

#[test]
fn verification_requires_exact_session_and_explicit_confirmation_before_apply() {
    let fixture = Fixture::new();
    let before = std::fs::read(fixture.directory.join("config.toml")).unwrap();
    let proof = fixture.verify();
    assert_eq!(proof.configuration_plan_id, fixture.plan.plan_id);
    assert!(!format!("{proof:?}").contains(&fixture.key.canonical()));
    assert!(fixture.apply(SESSION, None).is_err());
    let mut confirmation = proto::AdministratorConfirmation {
        verification_id: proof.verification_id,
        confirm: false,
    };
    assert!(fixture.apply(SESSION, Some(confirmation.clone())).is_err());
    confirmation.confirm = true;
    let other = crate::webrtc::SessionId::from_u64(126);
    bind_session(&fixture.state, other, &fixture.owner);
    assert!(fixture.apply(other, Some(confirmation.clone())).is_err());
    assert_eq!(
        std::fs::read(fixture.directory.join("config.toml")).unwrap(),
        before
    );
    fixture.apply(SESSION, Some(confirmation.clone())).unwrap();
    assert!(
        crate::config::load_config(&fixture.directory.join("config.toml"))
            .unwrap()
            .external_auth
            .is_none()
    );
    assert!(!super::super::active(
        &fixture.state,
        &fixture.owner,
        Instant::now(),
        super::super::now_ms()
    ));
    assert!(fixture.apply(SESSION, Some(confirmation)).is_err());
}

#[test]
fn configuration_apply_rechecks_revoked_actor_after_waiting_for_writer() {
    let mut fixture = Fixture::new();
    fixture.plan = preserving_plan(&fixture);
    assert!(!fixture.plan.requires_administrator_confirmation);
    let path = fixture.directory.join("config.toml");
    let before = std::fs::read(&path).unwrap();
    let update = fixture.state.config_update.lock().unwrap();
    let (started, waiting) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let apply = scope.spawn(|| {
            started.send(()).unwrap();
            fixture.apply(SESSION, None)
        });
        waiting
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let csrf = fixture
            .state
            .authentication
            .lock()
            .unwrap()
            .sessions
            .lookup(&fixture.cookie, "https://keeppeek.example", Instant::now())
            .unwrap()
            .csrf()
            .to_owned();
        let response = super::super::logout(
            &super::super::tests::remote_request("POST", &fixture.cookie, Some(&csrf)),
            &fixture.state,
        )
        .unwrap();
        assert_eq!(response.status_code, 204);
        drop(update);
        assert!(apply.join().unwrap().is_err());
    });
    assert_eq!(std::fs::read(path).unwrap(), before);
    assert!(
        !fixture
            .state
            .authentication
            .lock()
            .unwrap()
            .config
            .as_ref()
            .unwrap()
            .bearer_enabled
    );
}

fn revoke_browsers_before_commit(state: &ServerState) {
    let mut registry = state.authentication.lock().unwrap();
    let browsers: Vec<_> = registry.sessions.list().map(|browser| browser.id).collect();
    for browser in browsers {
        registry.sessions.revoke(browser);
    }
}

#[test]
fn configuration_commit_holds_browser_guard_through_persistence_and_activation() {
    let fixture = Fixture::new();
    let _update = fixture.state.config_update.lock().unwrap();
    let path = fixture.directory.join("config.toml");
    let config = crate::config::load_config(&path).unwrap();
    let next = super::super::prepare_configuration(&fixture.state, &config, &config).unwrap();
    let revision = fixture.state.authentication.lock().unwrap().revision;
    super::super::commit_configuration(&fixture.state, Some(&fixture.owner), Some(next), || {
        assert!(matches!(
            fixture.state.authentication.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        ));
        Ok(())
    })
    .unwrap();
    assert_eq!(
        fixture.state.authentication.lock().unwrap().revision,
        revision + 1
    );
}

#[test]
fn configuration_commit_rechecks_browser_revoked_after_preparation() {
    for confirmed in [false, true] {
        let mut fixture = Fixture::new();
        let confirmation = if confirmed {
            Some(proto::AdministratorConfirmation {
                verification_id: fixture.verify().verification_id,
                confirm: true,
            })
        } else {
            fixture.plan = preserving_plan(&fixture);
            None
        };
        let path = fixture.directory.join("config.toml");
        let before = std::fs::read(&path).unwrap();
        let revision = fixture.state.authentication.lock().unwrap().revision;
        super::super::BEFORE_CONFIGURATION_COMMIT
            .with(|hook| hook.set(Some(revoke_browsers_before_commit)));
        assert!(
            fixture.apply(SESSION, confirmation.clone()).is_err(),
            "confirmed={confirmed}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(
            fixture.state.authentication.lock().unwrap().revision,
            revision
        );
        if let Some(confirmation) = confirmation {
            assert!(
                fixture
                    .state
                    .configuration_plans
                    .proofs
                    .lock()
                    .unwrap()
                    .proofs
                    .contains_key(&uuid::Uuid::parse_str(&confirmation.verification_id).unwrap())
            );
        }
    }
}

#[test]
fn configuration_restore_cannot_remove_the_last_administrator_without_confirmation() {
    let mut fixture = Fixture::new();
    let archive = replacement_archive(&fixture);
    let path = fixture.directory.join("config.toml");
    fixture.state.backup_manager = Some(std::sync::Arc::new(
        crate::backup::BackupManager::open_with_config_update(
            path.clone(),
            fixture.state.config_update.clone(),
        )
        .unwrap(),
    ));
    let before = std::fs::read(&path).unwrap();
    let classification = fixture
        .state
        .api_session_owners
        .lock()
        .unwrap()
        .get(&SESSION)
        .unwrap()
        .classification;
    let response = crate::server::config_apply(
        &rouille::Request::fake_https_from(
            "203.0.113.1:443".parse().unwrap(),
            "POST",
            "/config/apply",
            vec![
                ("Content-Type".into(), "application/zip".into()),
                ("Content-Length".into(), archive.len().to_string()),
            ],
            archive,
        ),
        &fixture.state,
        &crate::server::AuthenticatedApiRequest {
            principal: fixture.owner.clone(),
            classification,
        },
    );
    assert_eq!(response.status_code, 409);
    assert_eq!(std::fs::read(path).unwrap(), before);
    assert!(
        crate::backup::active_restore(&fixture.directory.join("config.toml"), 1_788_000_000_001)
            .unwrap()
            .is_none()
    );
}

pub(super) fn replacement_archive(fixture: &Fixture) -> Vec<u8> {
    let source = fixture.directory.join("source");
    std::fs::create_dir(&source).unwrap();
    let candidate =
        configuration::verification_candidate(&fixture.state, &fixture.plan.plan_id).unwrap();
    let path = source.join("config.toml");
    crate::config::write_configuration_table(&path, &candidate.after.source).unwrap();
    crate::config::write_private_file(&source.join("secrets.toml"), b"").unwrap();
    crate::backup::create_bundle(
        std::io::Cursor::new(Vec::new()),
        crate::backup::CreateBundleOptions {
            config_path: &path,
            sections: &[],
            created_at_unix_ms: 1_788_000_000_000,
        },
    )
    .unwrap()
    .0
    .into_inner()
}

fn preserving_plan(fixture: &Fixture) -> proto::ConfigurationPlan {
    let root =
        crate::config::load_configuration_table(&fixture.directory.join("config.toml")).unwrap();
    let mut settings = crate::access::external_proto::from_root(&root)
        .unwrap()
        .unwrap();
    settings.bearer_enabled = true;
    settings.bearer_transition_until_ms = Some(super::super::now_ms() + 300_000);
    let result = configuration::dispatch_as(
        &fixture.state,
        SESSION,
        &fixture.owner,
        proto::ConfigurationCommand {
            action: Some(proto::configuration_command::Action::Plan(
                proto::PlanConfigurationChange {
                    expected_configuration_revision: fixture.plan.configuration_revision.clone(),
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
            )),
        },
    )
    .unwrap();
    let proto::ok::Result::ConfigurationResult(result) = result else {
        panic!("wrong result");
    };
    let Some(proto::configuration_result::Result::Plan(plan)) = result.result else {
        panic!("wrong plan");
    };
    plan
}

#[test]
fn verification_is_invalid_after_parent_close_or_secret_file_change() {
    for close_parent in [true, false] {
        let fixture = Fixture::new();
        let proof = fixture.verify();
        let before = std::fs::read(fixture.directory.join("config.toml")).unwrap();
        if close_parent {
            crate::server::close_api_session(&fixture.state, SESSION);
        } else {
            std::fs::write(
                fixture.directory.join("secrets.toml"),
                "UNRELATED = 'changed'\n",
            )
            .unwrap();
        }
        assert!(
            fixture
                .apply(
                    SESSION,
                    Some(proto::AdministratorConfirmation {
                        verification_id: proof.verification_id,
                        confirm: true,
                    })
                )
                .is_err()
        );
        assert_eq!(
            std::fs::read(fixture.directory.join("config.toml")).unwrap(),
            before
        );
    }
}

#[test]
fn verification_persistence_failure_preserves_evidence_and_live_sessions() {
    let fixture = Fixture::new();
    let proof = fixture.verify();
    let confirmation = proto::AdministratorConfirmation {
        verification_id: proof.verification_id.clone(),
        confirm: true,
    };
    let request = proto::ApplyConfigurationPlan {
        plan_id: fixture.plan.plan_id.clone(),
        expected_configuration_revision: fixture.plan.configuration_revision.clone(),
        administrator_confirmation: Some(confirmation.clone()),
    };
    let update = fixture.state.config_update.lock().unwrap();
    let candidate =
        configuration::verification_candidate(&fixture.state, &fixture.plan.plan_id).unwrap();
    let backup = fixture.directory.join("original.toml");
    let original = std::fs::read(&candidate.path).unwrap();
    std::fs::rename(&candidate.path, &backup).unwrap();
    std::fs::create_dir(&candidate.path).unwrap();
    let result = commit(
        &fixture.state,
        Some((SESSION, &fixture.owner)),
        &request,
        &candidate,
    );
    std::fs::remove_dir(&candidate.path).unwrap();
    std::fs::rename(&backup, &candidate.path).unwrap();
    assert_eq!(result.unwrap_err()._http_status, 503);
    assert_eq!(std::fs::read(&candidate.path).unwrap(), original);
    assert!(
        fixture
            .state
            .configuration_plans
            .proofs
            .lock()
            .unwrap()
            .proofs
            .contains_key(&uuid::Uuid::parse_str(&proof.verification_id).unwrap())
    );
    assert!(super::super::active(
        &fixture.state,
        &fixture.owner,
        Instant::now(),
        super::super::now_ms()
    ));
    drop(update);
    fixture.apply(SESSION, Some(confirmation)).unwrap();
}

fn synthetic_proof(session: u64, now: Instant, now_ms: i64) -> Proof {
    Proof {
        session: crate::webrtc::SessionId::from_u64(session),
        principal: crate::server::ApiPrincipalIdentity::Credential {
            id: uuid::Uuid::new_v4(),
            revision: 1,
        },
        plan_id: uuid::Uuid::new_v4().to_string(),
        revision: "revision".into(),
        fingerprint: [0; 32],
        stage: Stage::Bearer((uuid::Uuid::new_v4(), 1)),
        expires: now + super::PROOF_LIFETIME,
        expires_at_ms: now_ms + 300_000,
    }
}

#[test]
fn verification_capacity_is_bounded_without_evicting_live_proofs() {
    let now = Instant::now();
    let mut registry = Registry::default();
    let first = registry.insert(synthetic_proof(1, now, 0), now, 0).unwrap();
    registry.insert(synthetic_proof(1, now, 0), now, 0).unwrap();
    assert_eq!(
        registry
            .insert(synthetic_proof(1, now, 0), now, 0)
            .unwrap_err()
            ._http_status,
        429
    );
    for session in 2..32 {
        registry
            .insert(synthetic_proof(session, now, 0), now, 0)
            .unwrap();
    }
    assert_eq!(registry.proofs.len(), 32);
    assert!(
        registry
            .insert(synthetic_proof(33, now, 0), now, 0)
            .is_err()
    );
    assert!(
        registry
            .proofs
            .contains_key(&uuid::Uuid::parse_str(&first.verification_id).unwrap())
    );
    let later = now + super::PROOF_LIFETIME;
    registry
        .insert(synthetic_proof(33, later, 300_000), later, 300_000)
        .unwrap();
    assert_eq!(registry.proofs.len(), 1);
}

#[test]
fn verification_expiry_and_local_administration_cannot_authorize_a_transition() {
    let fixture = Fixture::new();
    let local = ApiPrincipal::local("127.0.0.1".parse().unwrap());
    let request = proto::VerifyAdministratorBearer {
        configuration_plan_id: fixture.plan.plan_id.clone(),
        access_key: fixture.key.canonical(),
    };
    assert!(!format!("{request:?}").contains(&request.access_key));
    assert!(
        verify_bearer(
            &fixture.state,
            crate::webrtc::SessionId::from_u64(0),
            &local,
            request
        )
        .is_err()
    );
    let proof = fixture.verify();
    fixture
        .state
        .configuration_plans
        .proofs
        .lock()
        .unwrap()
        .proofs
        .get_mut(&uuid::Uuid::parse_str(&proof.verification_id).unwrap())
        .unwrap()
        .expires = Instant::now();
    assert!(
        fixture
            .apply(
                SESSION,
                Some(proto::AdministratorConfirmation {
                    verification_id: proof.verification_id,
                    confirm: true
                })
            )
            .is_err()
    );
    assert!(super::super::active(
        &fixture.state,
        &fixture.owner,
        Instant::now(),
        super::super::now_ms()
    ));
}

#[test]
fn shadowed_administrator_cannot_bypass_confirmation_when_clearing_external_authentication() {
    let mut fixture = Fixture::new();
    let path = fixture.directory.join("config.toml");
    let mut root = crate::config::load_configuration_table(&path).unwrap();
    root["external_auth"]
        .as_table_mut()
        .unwrap()
        .insert("bearer_enabled".into(), true.into());
    root["external_auth"].as_table_mut().unwrap().insert(
        "bearer_transition_until_ms".into(),
        (super::super::now_ms() + 300_000).into(),
    );
    let credentials = root["access_credentials"]["credentials"]
        .as_array_mut()
        .unwrap();
    let mut shadow = credentials[0].clone();
    shadow["id"] = uuid::Uuid::new_v4().to_string().into();
    shadow["name"] = "Shadow".into();
    shadow["role"] = "user".into();
    credentials.push(shadow);
    crate::config::write_configuration_table(&path, &root).unwrap();
    fixture.state.authentication.lock().unwrap().config =
        crate::config::load_config(&path).unwrap().external_auth;
    fixture.state.access_manager = AccessManager::open_with_config_update(
        &path,
        AccessKey::unset(),
        fixture.state.config_update.clone(),
    )
    .unwrap();
    let credential = fixture
        .state
        .access_manager
        .authenticate(
            "203.0.113.1".parse().unwrap(),
            &[&format!("Bearer {}", fixture.key.canonical())],
            super::super::now_ms(),
            Instant::now(),
        )
        .unwrap();
    assert_eq!(credential.role, AccessRole::User);
    fixture.plan = clear_plan(&fixture.state, &fixture.owner);
    let before = std::fs::read(&path).unwrap();
    assert!(fixture.apply(SESSION, None).is_err());
    assert!(fixture.plan.requires_administrator_confirmation);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
