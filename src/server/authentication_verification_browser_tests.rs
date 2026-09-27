use super::super::tests::{Fixture, SESSION};
use super::*;

fn proxy_settings(origin: &str) -> proto::ExternalAuthenticationSettings {
    proto::ExternalAuthenticationSettings {
        allowed_origins: vec![origin.into()],
        providers: vec![proto::ExternalAuthenticationProvider {
            provider_id: "replacement".into(),
            name: "Replacement proxy".into(),
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

fn prepare_proxy(fixture: &mut Fixture, origin: &str) -> proto::AdministratorVerification {
    prepare_settings(fixture, origin, proxy_settings(origin))
}

fn prepare_settings(
    fixture: &mut Fixture,
    origin: &str,
    settings: proto::ExternalAuthenticationSettings,
) -> proto::AdministratorVerification {
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
        panic!("wrong result")
    };
    let Some(proto::configuration_result::Result::Plan(plan)) = result.result else {
        panic!("wrong plan")
    };
    fixture.plan = plan;
    prepare(
        &fixture.state,
        SESSION,
        &fixture.owner,
        proto::PrepareAdministratorVerification {
            configuration_plan_id: fixture.plan.plan_id.clone(),
            provider_id: "replacement".into(),
            origin: origin.into(),
        },
    )
    .unwrap()
}

#[test]
fn oidc_candidate_can_prepare_without_replacing_the_active_configuration() {
    let mut fixture = Fixture::new();
    let origin = "https://keeppeek.example";
    let mut settings = proxy_settings(origin);
    settings.providers[0].method = Some(proto::external_authentication_provider::Method::Oidc(
        proto::OidcAuthenticationSettings {
            issuer: "https://replacement.example".into(),
            client_id: "keeppeek".into(),
            redirect_uri: format!("{origin}/auth/callback"),
            scopes: vec!["openid".into()],
            display_name_claim: "name".into(),
            ..Default::default()
        },
    ));
    let before = std::fs::read(fixture.directory.join("config.toml")).unwrap();
    let proof = prepare_settings(&mut fixture, origin, settings);
    assert!(!proof.verified);
    assert_eq!(proof.browser_start.unwrap().origin, origin);
    assert_eq!(
        std::fs::read(fixture.directory.join("config.toml")).unwrap(),
        before
    );
    assert!(super::super::super::active(
        &fixture.state,
        &fixture.owner,
        Instant::now(),
        super::super::super::now_ms()
    ));
}

fn proxy_request(
    fixture: &Fixture,
    proof: &proto::AdministratorVerification,
    peer: &str,
    origin: &str,
    csrf: &str,
) -> rouille::Request {
    let mut form = url::form_urlencoded::Serializer::new(String::new());
    form.append_pair("candidate_plan_id", &proof.verification_id)
        .append_pair("provider_id", "replacement")
        .append_pair("csrf_token", csrf)
        .append_pair("return_path", "/");
    rouille::Request::fake_https_from(
        peer.parse().unwrap(),
        "POST",
        "/auth/login",
        vec![
            (
                "Content-Type".into(),
                "application/x-www-form-urlencoded".into(),
            ),
            (
                "Host".into(),
                proof
                    .browser_start
                    .as_ref()
                    .unwrap()
                    .origin
                    .strip_prefix("https://")
                    .unwrap()
                    .into(),
            ),
            ("Origin".into(), origin.into()),
            ("Sec-Fetch-Site".into(), "same-origin".into()),
            (
                "Cookie".into(),
                format!("__Host-keeppeek-session={}", fixture.cookie),
            ),
            ("X-Identity-Subject".into(), "synthetic-bob".into()),
            ("X-Identity-Role".into(), "admins".into()),
        ],
        form.finish().into_bytes(),
    )
}

#[test]
fn proxy_candidate_proof_preserves_the_browser_and_commits_the_prepared_identity_once() {
    let mut fixture = Fixture::new();
    let origin = "https://verification.example";
    let proof = prepare_proxy(&mut fixture, origin);
    assert!(!proof.verified);
    let before = std::fs::read(fixture.directory.join("config.toml")).unwrap();
    let confirmation = proto::AdministratorConfirmation {
        verification_id: proof.verification_id.clone(),
        confirm: true,
    };
    assert!(fixture.apply(SESSION, Some(confirmation.clone())).is_err());
    let csrf = &proof.browser_start.as_ref().unwrap().csrf_token;
    assert!(!format!("{proof:?}").contains(csrf));
    let response = super::super::super::handle(
        &proxy_request(&fixture, &proof, "203.0.113.1:4567", origin, csrf),
        &fixture.state,
    )
    .unwrap();
    assert_eq!(response.status_code, 204);
    assert!(
        !response
            .headers
            .iter()
            .any(|(name, _)| name == "Set-Cookie")
    );
    assert_eq!(
        std::fs::read(fixture.directory.join("config.toml")).unwrap(),
        before
    );
    assert!(super::super::super::active(
        &fixture.state,
        &fixture.owner,
        Instant::now(),
        super::super::super::now_ms()
    ));
    assert_verified(&fixture, &proof);
    assert_ne!(
        super::super::super::handle(
            &proxy_request(&fixture, &proof, "203.0.113.1:4567", origin, csrf),
            &fixture.state
        )
        .unwrap()
        .status_code,
        204
    );
    fixture.apply(SESSION, Some(confirmation)).unwrap();
    assert_committed_identity(&fixture);
}

fn assert_verified(fixture: &Fixture, proof: &proto::AdministratorVerification) {
    let status = get(
        &fixture.state,
        SESSION,
        &fixture.owner,
        proto::GetAdministratorVerification {
            verification_id: proof.verification_id.clone(),
        },
    )
    .unwrap();
    assert!(status.verified);
    assert!(status.browser_start.is_none());
}

fn assert_committed_identity(fixture: &Fixture) {
    let config = crate::config::load_config(&fixture.directory.join("config.toml")).unwrap();
    let directory = crate::access::identities::Directory::from_root(&config.source).unwrap();
    let identity = directory
        .records
        .iter()
        .find(|identity| identity.provider_id == "replacement")
        .unwrap();
    assert_eq!(identity.role, AccessRole::Administrator);
    assert_eq!(identity.revision, 1);
    assert_eq!(
        fixture.state.authentication.lock().unwrap().directory,
        directory
    );
    assert!(
        !std::fs::read_to_string(fixture.directory.join("config.toml"))
            .unwrap()
            .contains("synthetic-bob")
    );
}

#[test]
fn proxy_candidate_failed_persistence_keeps_both_directories_and_the_prepared_uuid() {
    let mut fixture = Fixture::new();
    let proof = prepare_proxy(&mut fixture, "https://keeppeek.example");
    let start = proof.browser_start.as_ref().unwrap();
    let response = super::super::super::handle(
        &proxy_request(
            &fixture,
            &proof,
            "203.0.113.1:4567",
            &start.origin,
            &start.csrf_token,
        ),
        &fixture.state,
    )
    .unwrap();
    assert_eq!(response.status_code, 204);
    let id = Uuid::parse_str(&proof.verification_id).unwrap();
    let prepared = match snapshot(&fixture.state, id).unwrap().stage {
        Stage::External(identity) => identity.identity().clone(),
        _ => panic!("identity not verified"),
    };
    let old_directory = fixture
        .state
        .authentication
        .lock()
        .unwrap()
        .directory
        .clone();
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
    assert_failed_commit(&fixture, &request, &candidate);
    assert_eq!(
        fixture.state.authentication.lock().unwrap().directory,
        old_directory
    );
    drop(update);
    assert_verified(&fixture, &proof);
    fixture.apply(SESSION, Some(confirmation)).unwrap();
    assert_committed_identity(&fixture);
    assert_eq!(
        fixture
            .state
            .authentication
            .lock()
            .unwrap()
            .directory
            .records
            .iter()
            .find(|identity| identity.id == prepared.id),
        Some(&prepared)
    );
}

fn assert_failed_commit(
    fixture: &Fixture,
    request: &proto::ApplyConfigurationPlan,
    candidate: &Candidate,
) {
    let original = std::fs::read(&candidate.path).unwrap();
    let backup = fixture.directory.join("original.toml");
    std::fs::rename(&candidate.path, &backup).unwrap();
    std::fs::create_dir(&candidate.path).unwrap();
    let result = commit(
        &fixture.state,
        Some((SESSION, &fixture.owner)),
        request,
        candidate,
    );
    std::fs::remove_dir(&candidate.path).unwrap();
    std::fs::rename(backup, &candidate.path).unwrap();
    assert_eq!(result.unwrap_err()._http_status, 503);
    assert_eq!(std::fs::read(&candidate.path).unwrap(), original);
}

#[test]
fn proxy_candidate_start_requires_exact_origin_challenge_and_immediate_peer() {
    for failure in ["origin", "csrf", "peer"] {
        let mut fixture = Fixture::new();
        let proof = prepare_proxy(&mut fixture, "https://keeppeek.example");
        let start = proof.browser_start.as_ref().unwrap();
        let response = super::super::super::handle(
            &proxy_request(
                &fixture,
                &proof,
                if failure == "peer" {
                    "198.51.100.1:4567"
                } else {
                    "203.0.113.1:4567"
                },
                if failure == "origin" {
                    "https://evil.example"
                } else {
                    &start.origin
                },
                if failure == "csrf" {
                    "wrong"
                } else {
                    &start.csrf_token
                },
            ),
            &fixture.state,
        )
        .unwrap();
        assert_eq!(response.status_code, 403, "{failure}");
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
        assert!(super::super::super::active(
            &fixture.state,
            &fixture.owner,
            Instant::now(),
            super::super::super::now_ms()
        ));
    }
}
