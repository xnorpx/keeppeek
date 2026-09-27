use super::super::tests::{Fixture, SESSION};
use super::*;
use crate::server::authentication as auth;
use auth::oidc_tests::{CandidateFault, CandidateProvider};

const ORIGIN: &str = "https://verification.example";

struct Login {
    proof: proto::AdministratorVerification,
    cookie: String,
    state: String,
}

fn configuration_revision(fixture: &Fixture) -> String {
    let current = configuration::dispatch_as(
        &fixture.state,
        SESSION,
        &fixture.owner,
        proto::ConfigurationCommand {
            action: Some(
                proto::configuration_command::Action::GetExternalAuthentication(
                    proto::GetExternalAuthenticationConfiguration {},
                ),
            ),
        },
    )
    .unwrap();
    let proto::ok::Result::ConfigurationResult(current) = current else {
        panic!("wrong result")
    };
    let Some(proto::configuration_result::Result::ExternalAuthentication(current)) = current.result
    else {
        panic!("wrong settings")
    };
    current.configuration_revision
}

fn candidate_settings(provider: &CandidateProvider) -> proto::ExternalAuthenticationSettings {
    proto::ExternalAuthenticationSettings {
        allowed_origins: vec![ORIGIN.into()],
        providers: vec![proto::ExternalAuthenticationProvider {
            provider_id: "replacement".into(),
            name: "Candidate provider".into(),
            mappings: vec![proto::ExternalRoleMapping {
                claim: "groups".into(),
                value: "admins".into(),
                role: proto::AccessRole::Administrator as i32,
                camera_access: None,
            }],
            method: Some(proto::external_authentication_provider::Method::Oidc(
                proto::OidcAuthenticationSettings {
                    issuer: provider.config.issuer.clone(),
                    client_id: provider.config.client_id.clone(),
                    redirect_uri: provider.config.redirect_uri.clone(),
                    scopes: provider.config.scopes.clone(),
                    display_name_claim: provider.config.display_name_claim.clone(),
                    private_networks: provider
                        .config
                        .private_networks
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                    ..Default::default()
                },
            )),
        }],
        ..Default::default()
    }
}

fn prepare(
    fixture: &mut Fixture,
    provider: &CandidateProvider,
) -> proto::AdministratorVerification {
    let result = configuration::dispatch_as(
        &fixture.state,
        SESSION,
        &fixture.owner,
        proto::ConfigurationCommand {
            action: Some(proto::configuration_command::Action::Plan(
                proto::PlanConfigurationChange {
                    expected_configuration_revision: configuration_revision(fixture),
                    targets: None,
                    change: Some(proto::ConfigurationChange {
                        change: Some(proto::configuration_change::Change::ExternalAuthentication(
                            proto::ExternalAuthenticationUpdate {
                                value: Some(proto::external_authentication_update::Value::Set(
                                    candidate_settings(provider),
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
    assert!(fixture.plan.requires_administrator_confirmation);
    let proof = browser::prepare(
        &fixture.state,
        SESSION,
        &fixture.owner,
        proto::PrepareAdministratorVerification {
            configuration_plan_id: fixture.plan.plan_id.clone(),
            provider_id: "replacement".into(),
            origin: ORIGIN.into(),
        },
    )
    .unwrap();
    let cache = fixture
        .state
        .configuration_plans
        .proofs
        .lock()
        .unwrap()
        .providers
        .clone();
    let now = Instant::now();
    cache
        .lock()
        .unwrap()
        .get(&provider.config, now, || {
            oidc::Provider::discover_with_budget(
                &provider.config,
                provider.transport.clone(),
                &fixture.state.oidc_issuer_budgets,
                now,
            )
        })
        .unwrap();
    proof
}

fn start_request(fixture: &Fixture, proof: &proto::AdministratorVerification) -> Request {
    let csrf = &proof.browser_start.as_ref().unwrap().csrf_token;
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("candidate_plan_id", &proof.verification_id)
        .append_pair("provider_id", "replacement")
        .append_pair("csrf_token", csrf)
        .append_pair("return_path", "/events")
        .finish();
    Request::fake_https_from(
        "203.0.113.1:4567".parse().unwrap(),
        "POST",
        "/auth/login",
        vec![
            ("Host".into(), "verification.example".into()),
            ("Origin".into(), ORIGIN.into()),
            ("Sec-Fetch-Site".into(), "same-origin".into()),
            (
                "Content-Type".into(),
                "application/x-www-form-urlencoded".into(),
            ),
            (
                "Cookie".into(),
                format!("{}={}", auth::SESSION_COOKIE, fixture.cookie),
            ),
        ],
        body.into_bytes(),
    )
}

fn start(
    fixture: &Fixture,
    proof: proto::AdministratorVerification,
    provider: &CandidateProvider,
    fault: CandidateFault,
) -> Login {
    let request = start_request(fixture, &proof);
    let response = auth::handle(&request, &fixture.state).unwrap();
    assert_eq!(response.status_code, 303);
    assert_session_cookie_unchanged(&response);
    let cookie = response
        .headers
        .iter()
        .find(|(name, value)| name == "Set-Cookie" && value.starts_with(auth::LOGIN_COOKIE))
        .unwrap()
        .1
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let authorization = url::Url::parse(
        &response
            .headers
            .iter()
            .find(|(name, _)| name == "Location")
            .unwrap()
            .1,
    )
    .unwrap();
    provider.prepare(&authorization, fault);
    let state = authorization
        .query_pairs()
        .find(|(name, _)| name == "state")
        .unwrap()
        .1
        .into_owned();
    assert!(
        auth::handle(&start_request(fixture, &proof), &fixture.state)
            .unwrap()
            .status_code
            >= 400,
        "start challenge replay"
    );
    Login {
        proof,
        cookie,
        state,
    }
}

fn callback_request(fixture: &Fixture, login: &Login) -> Request {
    Request::fake_https_from(
        "203.0.113.1:4567".parse().unwrap(),
        "GET",
        format!("/auth/callback?code=candidate-code&state={}", login.state),
        vec![
            ("Host".into(), "verification.example".into()),
            ("Sec-Fetch-Site".into(), "cross-site".into()),
            (
                "Cookie".into(),
                format!(
                    "{}; {}={}",
                    login.cookie,
                    auth::SESSION_COOKIE,
                    fixture.cookie
                ),
            ),
        ],
        vec![],
    )
}

fn status(
    fixture: &Fixture,
    login: &Login,
) -> Result<proto::AdministratorVerification, ControlCommandError> {
    browser::get(
        &fixture.state,
        SESSION,
        &fixture.owner,
        proto::GetAdministratorVerification {
            verification_id: login.proof.verification_id.clone(),
        },
    )
}

fn confirmation(login: &Login) -> proto::AdministratorConfirmation {
    proto::AdministratorConfirmation {
        verification_id: login.proof.verification_id.clone(),
        confirm: true,
    }
}

fn assert_session_cookie_unchanged(response: &Response) {
    assert!(
        !response
            .headers
            .iter()
            .any(|(name, value)| name == "Set-Cookie" && value.starts_with(auth::SESSION_COOKIE))
    );
}

fn disk(fixture: &Fixture) -> Vec<u8> {
    std::fs::read(fixture.directory.join("config.toml")).unwrap()
}

fn assert_unverified(fixture: &Fixture, login: &Login, expected: &[u8]) {
    assert!(status(fixture, login).map_or(true, |proof| !proof.verified));
    assert!(fixture.apply(SESSION, Some(confirmation(login))).is_err());
    assert_eq!(disk(fixture), expected);
}

#[test]
fn candidate_oidc_tls_prepare_start_callback_get_apply_is_proof_only_and_single_use() {
    let provider = CandidateProvider::new(ORIGIN);
    let mut fixture = Fixture::new();
    let before = disk(&fixture);
    let directory = fixture
        .state
        .authentication
        .lock()
        .unwrap()
        .directory
        .clone();
    let proof = prepare(&mut fixture, &provider);
    let login = start(&fixture, proof, &provider, CandidateFault::None);
    assert!(!status(&fixture, &login).unwrap().verified);
    assert!(fixture.apply(SESSION, Some(confirmation(&login))).is_err());
    let request = callback_request(&fixture, &login);
    let response = auth::handle(&request, &fixture.state).unwrap();
    assert_eq!(response.status_code, 303);
    assert_session_cookie_unchanged(&response);
    assert!(
        response
            .headers
            .iter()
            .any(|(name, value)| name == "Location" && value == "/events")
    );
    assert!(
        response
            .headers
            .iter()
            .any(|(name, value)| name == "Set-Cookie"
                && value.starts_with(auth::LOGIN_COOKIE)
                && value.contains("Max-Age=0"))
    );
    assert_eq!(provider.token_requests(), 1);
    assert_eq!(disk(&fixture), before);
    assert_eq!(
        fixture.state.authentication.lock().unwrap().directory,
        directory
    );
    assert!(auth::active(
        &fixture.state,
        &fixture.owner,
        Instant::now(),
        auth::now_ms()
    ));
    assert!(status(&fixture, &login).unwrap().verified);
    assert!(status(&fixture, &login).unwrap().browser_start.is_none());
    assert!(auth::handle(&request, &fixture.state).unwrap().status_code >= 400);
    assert_applied_identity(&fixture, &login);
}

fn assert_applied_identity(fixture: &Fixture, login: &Login) {
    let proof = browser::snapshot(
        &fixture.state,
        Uuid::parse_str(&login.proof.verification_id).unwrap(),
    )
    .unwrap();
    let Stage::External(prepared) = proof.stage else {
        panic!("identity not verified")
    };
    fixture.apply(SESSION, Some(confirmation(login))).unwrap();
    let root = config::load_config(&fixture.directory.join("config.toml")).unwrap();
    let saved = identities::Directory::from_root(&root.source).unwrap();
    assert_eq!(
        saved
            .records
            .iter()
            .find(|identity| identity.id == prepared.identity().id),
        Some(prepared.identity())
    );
    assert_eq!(
        fixture.state.authentication.lock().unwrap().directory,
        saved
    );
    assert!(!auth::active(
        &fixture.state,
        &fixture.owner,
        Instant::now(),
        auth::now_ms()
    ));
    assert!(fixture.apply(SESSION, Some(confirmation(login))).is_err());
    let saved = String::from_utf8(disk(fixture)).unwrap();
    for secret in [
        "candidate-access-token",
        "candidate-alice",
        &login.state,
        &login.cookie,
    ] {
        assert!(!saved.contains(secret));
    }
}

#[test]
fn candidate_oidc_tls_rejects_invalid_state_and_browser_before_exchange() {
    for bad_browser in [false, true] {
        let provider = CandidateProvider::new(ORIGIN);
        let mut fixture = Fixture::new();
        let before = disk(&fixture);
        let proof = prepare(&mut fixture, &provider);
        let mut login = start(&fixture, proof, &provider, CandidateFault::None);
        if bad_browser {
            login.cookie = format!("{}=wrong", auth::LOGIN_COOKIE);
        } else {
            login.state = "wrong-state".into();
        }
        let response = auth::handle(&callback_request(&fixture, &login), &fixture.state).unwrap();
        assert!(matches!(response.status_code, 401 | 403));
        assert_session_cookie_unchanged(&response);
        assert_eq!(provider.token_requests(), 0);
        assert_unverified(&fixture, &login, &before);
    }
}

#[test]
fn candidate_oidc_tls_rejects_bad_nonce_audience_signature_and_issuer() {
    for fault in [
        CandidateFault::Nonce,
        CandidateFault::Audience,
        CandidateFault::Signature,
        CandidateFault::Issuer,
    ] {
        let provider = CandidateProvider::new(ORIGIN);
        let mut fixture = Fixture::new();
        let before = disk(&fixture);
        let proof = prepare(&mut fixture, &provider);
        let login = start(&fixture, proof, &provider, fault);
        let request = callback_request(&fixture, &login);
        let response = auth::handle(&request, &fixture.state).unwrap();
        assert_eq!(response.status_code, 401, "{fault:?}");
        assert_session_cookie_unchanged(&response);
        assert_eq!(provider.token_requests(), 1);
        assert_unverified(&fixture, &login, &before);
        assert!(auth::active(
            &fixture.state,
            &fixture.owner,
            Instant::now(),
            auth::now_ms()
        ));
        assert!(auth::handle(&request, &fixture.state).unwrap().status_code >= 400);
        assert_eq!(provider.token_requests(), 1);
    }
}

fn restrict_directory(fixture: &Fixture, provider: &CandidateProvider, full: bool) {
    let _update = fixture.state.config_update.lock().unwrap();
    let mut registry = fixture.state.authentication.lock().unwrap();
    let mut directory = registry.directory.clone();
    for index in 0..if full { 1_023 } else { 1 } {
        let subject = if full {
            format!("capacity-{index}")
        } else {
            "candidate-alice".into()
        };
        let identity = directory
            .provision(identities::IdentityInput {
                provider_id: "replacement",
                namespace: &provider.config.issuer,
                subject: &subject,
                display_name: "Existing identity",
                grant: external::Grant {
                    role: AccessRole::Administrator,
                    camera_access: crate::access::CameraAccess::unrestricted(),
                },
                now_ms: auth::now_ms(),
            })
            .unwrap();
        if !full {
            directory.revoke(identity.id).unwrap();
        }
    }
    auth::save_directory(&fixture.state, &mut registry, directory).unwrap();
}

#[test]
fn candidate_oidc_tls_jit_rejects_disabled_subject_and_full_directory() {
    for full in [false, true] {
        let provider = CandidateProvider::new(ORIGIN);
        let mut fixture = Fixture::new();
        restrict_directory(&fixture, &provider, full);
        let before = disk(&fixture);
        let directory = fixture
            .state
            .authentication
            .lock()
            .unwrap()
            .directory
            .clone();
        let proof = prepare(&mut fixture, &provider);
        let login = start(&fixture, proof, &provider, CandidateFault::None);
        let response = auth::handle(&callback_request(&fixture, &login), &fixture.state).unwrap();
        assert!(matches!(response.status_code, 401 | 403));
        assert_session_cookie_unchanged(&response);
        assert_eq!(provider.token_requests(), 1);
        assert_unverified(&fixture, &login, &before);
        assert_eq!(
            fixture.state.authentication.lock().unwrap().directory,
            directory
        );
    }
}

fn logout_parent(fixture: &Fixture) {
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
    let request = Request::fake_https_from(
        "203.0.113.1:4567".parse().unwrap(),
        "POST",
        "/auth/logout",
        vec![
            ("Host".into(), "keeppeek.example".into()),
            ("Origin".into(), "https://keeppeek.example".into()),
            ("X-KeepPeek-CSRF".into(), csrf),
            (
                "Cookie".into(),
                format!("{}={}", auth::SESSION_COOKIE, fixture.cookie),
            ),
        ],
        vec![],
    );
    assert_eq!(
        auth::handle(&request, &fixture.state).unwrap().status_code,
        204
    );
}

#[test]
fn candidate_oidc_tls_parent_logout_or_config_change_after_token_response_prevents_apply() {
    for logout in [false, true] {
        let provider = CandidateProvider::new(ORIGIN);
        let mut fixture = Fixture::new();
        let proof = prepare(&mut fixture, &provider);
        let login = start(&fixture, proof, &provider, CandidateFault::None);
        let (arrived, release) = provider.block_refresh();
        std::thread::scope(|scope| {
            let request = callback_request(&fixture, &login);
            let state = &fixture.state;
            let callback = scope.spawn(move || auth::handle(&request, state).unwrap());
            arrived
                .recv_timeout(Duration::from_secs(10))
                .expect("post-token JWKS request");
            if logout {
                logout_parent(&fixture);
            } else {
                let _update = fixture.state.config_update.lock().unwrap();
                let path = fixture.directory.join("config.toml");
                let mut root = config::load_configuration_table(&path).unwrap();
                root.insert(
                    "operator_note".into(),
                    "changed after token response".into(),
                );
                config::write_configuration_table(&path, &root).unwrap();
            }
            let after_mutation = disk(&fixture);
            release.send(()).unwrap();
            let response = callback.join().unwrap();
            assert!(response.status_code >= 400);
            assert_session_cookie_unchanged(&response);
            assert_unverified(&fixture, &login, &after_mutation);
        });
        assert_eq!(provider.token_requests(), 1);
    }
}
