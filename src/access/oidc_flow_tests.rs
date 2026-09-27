use super::{
    Exchange, Provider,
    tests::{claims, sign_generation},
};
use crate::access::{
    external::Oidc,
    login_transactions::{Start, Transactions},
    oidc_fixture::{Fixture, Request, Response},
    oidc_transport::Transport,
};
use oauth2::{CsrfToken as Nonce, PkceCodeChallenge, PkceCodeVerifier};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};

#[derive(Default)]
struct ProviderState {
    origin: String,
    nonce: String,
    challenge: String,
    code_used: bool,
    outage: bool,
    jwks_outage: bool,
    generation: u8,
    jwks_requests: usize,
    now_ms: Option<i64>,
    browser_origin: Option<String>,
    browser_group: Option<String>,
}

pub struct FixtureProvider {
    pub config: Oidc,
    pub transport: Transport,
    state: Arc<Mutex<ProviderState>>,
    _fixture: Fixture,
}

impl FixtureProvider {
    pub(crate) fn new() -> Self {
        let state = Arc::new(Mutex::new(ProviderState::default()));
        let server_state = state.clone();
        let fixture =
            Fixture::new(move |request| serve(&mut server_state.lock().unwrap(), request));
        state.lock().unwrap().origin = fixture.origin.clone();
        let config: Oidc = toml::from_str(&format!("issuer = '{}'\nclient_id = 'keeppeek'\nredirect_uri = 'https://keeppeek.example/auth/callback'\nprivate_networks = ['127.0.0.1/32']", fixture.origin)).unwrap();
        let transport = Transport::for_test(&config, fixture.certificate.clone()).unwrap();
        Self {
            config,
            transport,
            state,
            _fixture: fixture,
        }
    }

    pub(crate) fn prepare_login(&self, url: &url::Url, now_ms: i64) {
        let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
        let mut state = self.state.lock().unwrap();
        state.nonce = query["nonce"].clone();
        state.challenge = query["code_challenge"].clone();
        state.now_ms = Some(now_ms);
        state.code_used = false;
    }

    pub(crate) fn set_outage(&self) {
        self.state.lock().unwrap().outage = true;
    }

    pub(crate) fn for_browser(origin: &str) -> Self {
        let state = Arc::new(Mutex::new(ProviderState {
            browser_origin: Some(origin.into()),
            browser_group: Some("viewers".into()),
            ..Default::default()
        }));
        let server_state = state.clone();
        let fixture =
            Fixture::for_browser(move |request| serve(&mut server_state.lock().unwrap(), request));
        state.lock().unwrap().origin = fixture.origin.clone();
        let config: Oidc = toml::from_str(&format!(
            "issuer = '{}'\nclient_id = 'keeppeek'\nredirect_uri = '{origin}/auth/callback'\nprivate_networks = ['127.0.0.1/32']",
            fixture.origin,
        )).unwrap();
        let transport = Transport::for_test(&config, fixture.certificate.clone()).unwrap();
        Self {
            config,
            transport,
            state,
            _fixture: fixture,
        }
    }

    pub(crate) fn browser_mode(&self, available: bool, administrator: bool) {
        let mut state = self.state.lock().unwrap();
        state.outage = !available;
        state.browser_group = Some(
            if administrator {
                "administrators"
            } else {
                "viewers"
            }
            .into(),
        );
    }
}

fn serve(state: &mut ProviderState, request: Request) -> Response {
    if state.outage || (state.jwks_outage && request.path == "/jwks") {
        return Response {
            status: 503,
            headers: vec![],
            body: b"private upstream failure".to_vec(),
        };
    }
    if state.browser_origin.is_some() && request.path.starts_with("/authorize?") {
        return authorize_browser(state, &request.path);
    }
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/.well-known/openid-configuration") => Response::json(&serde_json::json!({
            "issuer": state.origin, "authorization_endpoint": format!("{}/authorize", state.origin),
            "token_endpoint": format!("{}/token", state.origin), "jwks_uri": format!("{}/jwks", state.origin),
            "response_types_supported": ["code"], "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["EdDSA"], "code_challenge_methods_supported": ["S256"]
        })),
        ("GET", "/jwks") => {
            state.jwks_requests += 1;
            Response::json(
                &serde_json::to_value(sign_generation(claims(), state.generation).1).unwrap(),
            )
        }
        ("POST", "/token") => token(state, request),
        _ => Response {
            status: 404,
            headers: vec![],
            body: vec![],
        },
    }
}

fn authorize_browser(state: &mut ProviderState, path: &str) -> Response {
    let origin = state.browser_origin.as_ref().unwrap();
    let url = url::Url::parse(&format!("{}{path}", state.origin)).unwrap();
    let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["redirect_uri"], format!("{origin}/auth/callback"));
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["client_id"], "keeppeek");
    state.nonce = query["nonce"].clone();
    state.challenge = query["code_challenge"].clone();
    state.now_ms = Some(chrono::Utc::now().timestamp_millis());
    state.code_used = false;
    let mut callback = url::Url::parse(&format!("{origin}/auth/callback")).unwrap();
    callback
        .query_pairs_mut()
        .append_pair("code", "synthetic-code")
        .append_pair("state", &query["state"]);
    Response {
        status: 303,
        headers: vec![
            ("Location".into(), callback.into()),
            ("Referrer-Policy".into(), "no-referrer".into()),
        ],
        body: vec![],
    }
}

#[test]
fn rotated_key_outage_keeps_the_unavailable_error_category() {
    let fixture = FixtureProvider::new();
    let provider = Provider::discover(&fixture.config, fixture.transport.clone()).unwrap();
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    {
        let mut state = fixture.state.lock().unwrap();
        state.generation = 1;
        state.jwks_outage = true;
        state.challenge = challenge.as_str().to_owned();
        state.nonce = "rotation-nonce".into();
    }
    let error = provider
        .exchange(
            Exchange {
                code: "synthetic-code",
                nonce: &Nonce::new("rotation-nonce".into()),
                verifier,
            },
            || 1_800_000_010_000,
        )
        .err()
        .unwrap();
    assert!(error.downcast_ref::<super::ProviderUnavailable>().is_some());
    assert!(!format!("{error:?}").contains("private upstream failure"));
}

#[test]
fn exchange_checks_token_expiry_after_the_response_arrives() {
    let fixture = FixtureProvider::new();
    let provider = Provider::discover(&fixture.config, fixture.transport.clone()).unwrap();
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    {
        let mut state = fixture.state.lock().unwrap();
        state.challenge = challenge.as_str().to_owned();
        state.nonce = "expiry-nonce".into();
    }
    let result = provider.exchange(
        Exchange {
            code: "synthetic-code",
            nonce: &Nonce::new("expiry-nonce".into()),
            verifier,
        },
        || {
            if fixture.state.lock().unwrap().code_used {
                1_800_000_121_000
            } else {
                1_800_000_010_000
            }
        },
    );
    assert!(result.is_err());
    assert!(fixture.state.lock().unwrap().code_used);
}

fn token(state: &mut ProviderState, request: Request) -> Response {
    assert!(
        request
            .headers
            .get("content-type")
            .is_some_and(|value| value.starts_with("application/x-www-form-urlencoded"))
    );
    let values: HashMap<_, _> = url::form_urlencoded::parse(&request.body)
        .into_owned()
        .collect();
    let verifier = values.get("code_verifier").cloned().unwrap_or_default();
    let challenge = PkceCodeChallenge::from_code_verifier_sha256(&PkceCodeVerifier::new(verifier));
    let valid = !state.code_used
        && values
            .get("code")
            .is_some_and(|code| code == "synthetic-code")
        && values
            .get("grant_type")
            .is_some_and(|value| value == "authorization_code")
        && values
            .get("client_id")
            .is_some_and(|value| value == "keeppeek")
        && values.get("redirect_uri").is_some_and(|value| {
            value
                == &format!(
                    "{}/auth/callback",
                    state
                        .browser_origin
                        .as_deref()
                        .unwrap_or("https://keeppeek.example")
                )
        })
        && challenge.as_str() == state.challenge;
    if !valid {
        return Response {
            status: 400,
            headers: vec![],
            body: br#"{"error":"invalid_grant"}"#.to_vec(),
        };
    }
    state.code_used = true;
    let mut claims = claims();
    claims["iss"] = state.origin.clone().into();
    claims["nonce"] = state.nonce.clone().into();
    if let Some(group) = &state.browser_group {
        claims["groups"] = serde_json::json!([group]);
        claims["sub"] = "synthetic-provider-subject-private".into();
        claims["name"] = "Fixture User".into();
    }
    if let Some(now_ms) = state.now_ms {
        claims["iat"] = (now_ms / 1_000).into();
        claims["exp"] = (now_ms / 1_000 + 120).into();
    }
    Response::json(
        &serde_json::json!({"access_token": "synthetic-access-token", "token_type": "Bearer", "expires_in": 120, "id_token": sign_generation(claims, state.generation).0}),
    )
}

#[test]
fn https_oidc_fixture_exercises_discovery_pkce_code_exchange_replay_and_outage() {
    let state = Arc::new(Mutex::new(ProviderState::default()));
    let server_state = state.clone();
    let fixture = Fixture::new(move |request| serve(&mut server_state.lock().unwrap(), request));
    state.lock().unwrap().origin = fixture.origin.clone();
    let config: Oidc = toml::from_str(&format!("issuer = '{}'\nclient_id = 'keeppeek'\nredirect_uri = 'https://keeppeek.example/auth/callback'\nprivate_networks = ['127.0.0.1/32']", fixture.origin)).unwrap();
    let transport = Transport::for_test(&config, fixture.certificate.clone()).unwrap();
    let provider = Provider::discover(&config, transport.clone()).unwrap();
    let browser = uuid::Uuid::new_v4();
    let now = Instant::now();
    let mut transactions = Transactions::default();
    let url = transactions
        .start(
            Start {
                browser,
                origin: "https://keeppeek.example",
                provider_id: "company",
                revision: 1,
                return_path: "/",
                candidate_plan: None,
            },
            &config,
            provider.authorization_endpoint(),
            now,
        )
        .unwrap();
    let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
    state.lock().unwrap().nonce = query["nonce"].clone();
    state.lock().unwrap().challenge = query["code_challenge"].clone();
    let transaction = transactions
        .consume(&query["state"], browser, "https://keeppeek.example", 1, now)
        .unwrap();
    let nonce = Nonce::new(query["nonce"].clone());
    assert!(
        exchange_test_code(
            &provider,
            &nonce,
            PkceCodeVerifier::new("wrong-verifier".repeat(4))
        )
        .is_err()
    );
    let repeated = PkceCodeVerifier::new(transaction.verifier.secret().clone());
    let verified = exchange_test_code(&provider, &transaction.nonce, transaction.verifier).unwrap();
    assert_eq!(verified.subject, "alice");
    assert_eq!(verified.claims["groups"], serde_json::json!(["viewers"]));
    assert!(exchange_test_code(&provider, &nonce, repeated).is_err());
    state.lock().unwrap().outage = true;
    let error = exchange_test_code(
        &provider,
        &nonce,
        PkceCodeVerifier::new("synthetic-verifier".repeat(4)),
    )
    .err()
    .unwrap();
    assert!(error.downcast_ref::<super::ProviderUnavailable>().is_some());
    let error = Provider::discover(&config, transport).err().unwrap();
    assert!(!format!("{error:?}").contains("private upstream failure"));
}

fn exchange_test_code(
    provider: &Provider,
    nonce: &Nonce,
    verifier: PkceCodeVerifier,
) -> anyhow::Result<super::Verified> {
    provider.exchange(
        Exchange {
            code: "synthetic-code",
            nonce,
            verifier,
        },
        || 1_800_000_010_000,
    )
}

#[test]
fn key_rotation_refreshes_once_and_metadata_refresh_is_bounded() {
    let state = Arc::new(Mutex::new(ProviderState::default()));
    let server_state = state.clone();
    let fixture = Fixture::new(move |request| serve(&mut server_state.lock().unwrap(), request));
    state.lock().unwrap().origin = fixture.origin.clone();
    let config: Oidc = toml::from_str(&format!("issuer = '{}'\nclient_id = 'keeppeek'\nredirect_uri = 'https://keeppeek.example/auth/callback'\nprivate_networks = ['127.0.0.1/32']", fixture.origin)).unwrap();
    let transport = Transport::for_test(&config, fixture.certificate.clone()).unwrap();
    let mut cache = super::Cache::default();
    let now = Instant::now();
    let provider = cache
        .get(1, "company", now, || {
            Provider::discover(&config, transport.clone())
        })
        .unwrap();
    assert!(
        cache
            .get(1, "company", now, || panic!("cached discovery repeated"))
            .is_ok()
    );
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    {
        let mut state = state.lock().unwrap();
        state.generation = 1;
        state.challenge = challenge.as_str().to_owned();
        state.nonce = "rotation-nonce".into();
    }
    let retry = PkceCodeVerifier::new(verifier.secret().clone());
    let nonce = Nonce::new("rotation-nonce".into());
    assert!(exchange_test_code(&provider, &nonce, verifier).is_ok());
    assert_eq!(state.lock().unwrap().jwks_requests, 2);
    {
        let mut state = state.lock().unwrap();
        state.generation = 2;
        state.code_used = false;
    }
    assert!(exchange_test_code(&provider, &nonce, retry).is_err());
    assert_eq!(state.lock().unwrap().jwks_requests, 2);
    state.lock().unwrap().outage = true;
    assert!(
        cache
            .get(
                1,
                "company",
                now + std::time::Duration::from_secs(300),
                || Provider::discover(&config, transport)
            )
            .is_err()
    );
    assert!(
        cache
            .get(
                1,
                "company",
                now + std::time::Duration::from_secs(301),
                || panic!("outage refresh was not bounded")
            )
            .is_err()
    );
}
