use super::{
    tests::{browser_state, remote_request},
    *,
};
use crate::access::AccessRole;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{
    Algorithm, EncodingKey, Header,
    jwk::{Jwk, JwkSet},
};
use sha2::Sha512;

#[derive(Clone, Copy, Debug)]
pub(super) enum CandidateFault {
    None,
    Nonce,
    Audience,
    Signature,
    Issuer,
}

pub(super) struct CandidateProvider {
    pub(super) config: external::Oidc,
    pub(super) transport: crate::access::oidc_transport::Transport,
    state: std::sync::Arc<std::sync::Mutex<CandidateProviderState>>,
    _https: crate::access::oidc_fixture::Fixture,
}

struct CandidateProviderState {
    issuer: String,
    redirect_uri: String,
    nonce: String,
    challenge: String,
    fault: CandidateFault,
    key: (EncodingKey, Jwk),
    rotated_key: (EncodingKey, Jwk),
    rotate: bool,
    token_requests: u32,
    jwks_requests: u32,
    gate: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
}

impl CandidateProvider {
    pub(super) fn new(origin: &str) -> Self {
        let state = std::sync::Arc::new(std::sync::Mutex::new(CandidateProviderState {
            issuer: String::new(),
            redirect_uri: format!("{origin}/auth/callback"),
            nonce: String::new(),
            challenge: String::new(),
            fault: CandidateFault::None,
            key: candidate_signing_key("candidate-key"),
            rotated_key: candidate_signing_key("candidate-rotated"),
            rotate: false,
            token_requests: 0,
            jwks_requests: 0,
            gate: None,
        }));
        let served = state.clone();
        let https = crate::access::oidc_fixture::Fixture::new(move |request| {
            serve_candidate(&mut served.lock().unwrap(), request)
        });
        state.lock().unwrap().issuer = https.origin.clone();
        let config = toml::from_str(&format!(
            "issuer = '{}'\nclient_id = 'keeppeek'\nredirect_uri = '{origin}/auth/callback'\nprivate_networks = ['127.0.0.1/32']",
            https.origin,
        )).unwrap();
        let transport =
            crate::access::oidc_transport::Transport::for_test(&config, https.certificate.clone())
                .unwrap();
        Self {
            config,
            transport,
            state,
            _https: https,
        }
    }

    pub(super) fn prepare(&self, authorization: &url::Url, fault: CandidateFault) {
        let query: std::collections::HashMap<_, _> =
            authorization.query_pairs().into_owned().collect();
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(query["response_type"], "code");
        let mut state = self.state.lock().unwrap();
        state.nonce = query["nonce"].clone();
        state.challenge = query["code_challenge"].clone();
        state.fault = fault;
    }

    pub(super) fn block_refresh(
        &self,
    ) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        let (arrived, waiting) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let mut state = self.state.lock().unwrap();
        state.rotate = true;
        state.gate = Some((arrived, released));
        (waiting, release)
    }

    pub(super) fn token_requests(&self) -> u32 {
        self.state.lock().unwrap().token_requests
    }
}

fn candidate_signing_key(id: &str) -> (EncodingKey, Jwk) {
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
    // rcgen emits PKCS#8 v2; Jwk::from_encoding_key only accepts the 48-byte v1 form.
    let jwk = serde_json::from_value(serde_json::json!({
        "kty": "OKP", "crv": "Ed25519", "alg": "EdDSA", "kid": id,
        "x": URL_SAFE_NO_PAD.encode(key.public_key_raw()),
    }))
    .unwrap();
    (EncodingKey::from_ed_der(&key.serialize_der()), jwk)
}

fn serve_candidate(
    state: &mut CandidateProviderState,
    request: crate::access::oidc_fixture::Request,
) -> crate::access::oidc_fixture::Response {
    use crate::access::oidc_fixture::Response;
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/.well-known/openid-configuration") => Response::json(&serde_json::json!({
            "issuer": state.issuer, "authorization_endpoint": format!("{}/authorize", state.issuer),
            "token_endpoint": format!("{}/token", state.issuer), "jwks_uri": format!("{}/jwks", state.issuer),
            "response_types_supported": ["code"], "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["EdDSA"], "code_challenge_methods_supported": ["S256"]
        })),
        ("GET", "/jwks") => {
            state.jwks_requests += 1;
            if state.jwks_requests > 1
                && let Some((arrived, released)) = state.gate.take()
            {
                // This request proves the client received and parsed the real token response.
                arrived.send(()).unwrap();
                released
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .expect("release JWKS response");
            }
            let key = if state.rotate {
                &state.rotated_key
            } else {
                &state.key
            };
            Response::json(
                &serde_json::to_value(JwkSet {
                    keys: vec![key.1.clone()],
                })
                .unwrap(),
            )
        }
        ("POST", "/token") => candidate_token(state, request),
        _ => Response {
            status: 404,
            headers: vec![],
            body: vec![],
        },
    }
}

fn candidate_token(
    state: &mut CandidateProviderState,
    request: crate::access::oidc_fixture::Request,
) -> crate::access::oidc_fixture::Response {
    use oauth2::{PkceCodeChallenge, PkceCodeVerifier};
    let form: std::collections::HashMap<_, _> = url::form_urlencoded::parse(&request.body)
        .into_owned()
        .collect();
    assert_eq!(form["code"], "candidate-code");
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["client_id"], "keeppeek");
    assert_eq!(form["redirect_uri"], state.redirect_uri);
    assert_eq!(
        PkceCodeChallenge::from_code_verifier_sha256(&PkceCodeVerifier::new(
            form["code_verifier"].clone()
        ),)
        .as_str(),
        state.challenge
    );
    state.token_requests += 1;
    assert_eq!(
        state.token_requests, 1,
        "authorization code must be consumed once"
    );
    let now = now_ms() / 1_000;
    let mut claims = serde_json::json!({
        "iss": state.issuer, "aud": "keeppeek", "sub": "candidate-alice", "nonce": state.nonce,
        "iat": now, "exp": now + 120, "groups": ["admins"], "name": "Candidate Alice",
        "at_hash": URL_SAFE_NO_PAD.encode(&Sha512::digest(b"candidate-access-token")[..32]),
    });
    match state.fault {
        CandidateFault::Nonce => claims["nonce"] = "wrong-nonce".into(),
        CandidateFault::Audience => claims["aud"] = "wrong-client".into(),
        CandidateFault::Issuer => claims["iss"] = "https://wrong-issuer.example".into(),
        _ => {}
    }
    let key = if state.rotate {
        &state.rotated_key
    } else {
        &state.key
    };
    let mut header = Header::new(Algorithm::EdDSA);
    header.kid = key.1.common.key_id.clone();
    let mut token = jsonwebtoken::encode(&header, &claims, &key.0).unwrap();
    if matches!(state.fault, CandidateFault::Signature) {
        let (payload, signature) = token.rsplit_once('.').unwrap();
        let mut bytes = URL_SAFE_NO_PAD.decode(signature).unwrap();
        bytes[0] ^= 1;
        token = format!("{payload}.{}", URL_SAFE_NO_PAD.encode(bytes));
    }
    crate::access::oidc_fixture::Response::json(&serde_json::json!({
        "access_token": "candidate-access-token", "token_type": "Bearer", "expires_in": 120, "id_token": token,
    }))
}

fn ordinary_oidc_state(
    fixture: &crate::access::oidc::FixtureProvider,
) -> (ServerState, std::path::PathBuf) {
    let (mut state, _, _) = browser_state();
    let directory = std::env::temp_dir().join(format!("keeppeek-oidc-{}", Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("config.toml");
    let mut registry = state.authentication.lock().unwrap();
    registry.directory = Directory::default();
    let config = registry.config.as_mut().unwrap();
    config.providers[0].method = external::Method::Oidc(fixture.config.clone());
    config.providers[0].mappings[0].value = "viewers".into();
    let mut root = toml::Table::new();
    root.insert(
        "external_auth".into(),
        toml::Value::try_from(config.clone()).unwrap(),
    );
    crate::config::write_configuration_table(&path, &root).unwrap();
    drop(registry);
    state.camera_config_path = Some(path);
    state
        .oidc_cache
        .lock()
        .unwrap()
        .get(1, "company", Instant::now(), || {
            crate::access::oidc::Provider::discover(&fixture.config, fixture.transport.clone())
        })
        .unwrap();
    (state, directory)
}

fn ordinary_oidc_start(state: &ServerState) -> (String, url::Url) {
    let bootstrap_request = Request::fake_https_from(
        "203.0.113.1:4567".parse().unwrap(),
        "GET",
        "/auth/session",
        vec![("Host".into(), "keeppeek.example".into())],
        vec![],
    );
    let response = handle(&bootstrap_request, state).unwrap();
    let login_cookie = response
        .headers
        .iter()
        .find(|(name, value)| name == "Set-Cookie" && value.starts_with(LOGIN_COOKIE))
        .unwrap()
        .1
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let body: serde_json::Value =
        serde_json::from_reader(response.data.into_reader_and_size().0).unwrap();
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("provider_id", "company")
        .append_pair("csrf_token", body["csrf_token"].as_str().unwrap())
        .finish();
    let request = Request::fake_https_from(
        "203.0.113.1:4567".parse().unwrap(),
        "POST",
        "/auth/login",
        vec![
            ("Host".into(), "keeppeek.example".into()),
            ("Origin".into(), "https://keeppeek.example".into()),
            ("Cookie".into(), login_cookie.clone()),
            (
                "Content-Type".into(),
                "application/x-www-form-urlencoded".into(),
            ),
        ],
        form.into_bytes(),
    );
    let response = handle(&request, state).unwrap();
    assert_eq!(response.status_code, 303);
    let authorization = url::Url::parse(
        &response
            .headers
            .iter()
            .find(|(name, _)| name == "Location")
            .unwrap()
            .1,
    )
    .unwrap();
    (login_cookie, authorization)
}

#[test]
fn browser_oidc_login_callback_rotates_cookie_and_survives_provider_outage() {
    let fixture = crate::access::oidc::FixtureProvider::new();
    let (state, directory) = ordinary_oidc_state(&fixture);
    let (login_cookie, authorization) = ordinary_oidc_start(&state);
    fixture.prepare_login(&authorization, now_ms());
    let login_state = authorization
        .query_pairs()
        .find(|(name, _)| name == "state")
        .unwrap()
        .1
        .into_owned();
    let callback = Request::fake_https_from(
        "203.0.113.1:4567".parse().unwrap(),
        "GET",
        format!("/auth/callback?code=synthetic-code&state={login_state}"),
        vec![
            ("Host".into(), "keeppeek.example".into()),
            ("Cookie".into(), login_cookie),
            ("Sec-Fetch-Site".into(), "cross-site".into()),
        ],
        vec![],
    );
    let response = handle(&callback, &state).unwrap();
    assert_eq!(response.status_code, 303);
    let cookie = response
        .headers
        .iter()
        .find(|(name, value)| name == "Set-Cookie" && value.starts_with(SESSION_COOKIE))
        .unwrap()
        .1
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1
        .to_owned();
    fixture.set_outage();
    let principal = super::super::api_principal(&remote_request("GET", &cookie, None), &state)
        .unwrap()
        .principal;
    assert_eq!(principal.role, AccessRole::User);
    assert!(handle(&callback, &state).unwrap().status_code >= 400);
    assert!(
        !std::fs::read_to_string(directory.join("config.toml"))
            .unwrap()
            .contains("synthetic-access-token")
    );
    std::fs::remove_dir_all(directory).unwrap();
}
