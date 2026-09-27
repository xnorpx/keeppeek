use super::*;
use crate::access::oidc_fixture::{Fixture, Response};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

fn discovery_with(change: impl FnOnce(&mut Value)) -> Result<(Metadata, JwkSet)> {
    let document = Arc::new(Mutex::new(Value::Null));
    let served = document.clone();
    let fixture = Fixture::new(move |request| {
        if request.path == "/jwks" {
            return Response::json(&json!({"keys": [{
                "kty": "OKP", "crv": "Ed25519", "x": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
            }]}));
        }
        Response::json(&served.lock().unwrap())
    });
    let mut metadata = json!({
        "issuer": fixture.origin,
        "authorization_endpoint": format!("{}/authorize", fixture.origin),
        "token_endpoint": format!("{}/token", fixture.origin),
        "jwks_uri": format!("{}/jwks", fixture.origin),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["EdDSA"],
        "code_challenge_methods_supported": ["S256"]
    });
    change(&mut metadata);
    *document.lock().unwrap() = metadata;
    let config: Oidc = toml::from_str(&format!(
        "issuer = '{}'\nclient_id = 'keeppeek'\nredirect_uri = 'https://keeppeek.example/auth/callback'\nprivate_networks = ['127.0.0.1/32']",
        fixture.origin
    )).unwrap();
    let transport = Transport::for_test(&config, fixture.certificate.clone()).unwrap();
    Metadata::discover(&config, &transport)
}

#[test]
fn discovery_requires_exact_issuer_and_supported_code_pkce_and_signatures() {
    assert!(discovery_with(|_| {}).is_ok());
    for (field, value) in [
        ("issuer", json!("https://wrong.example")),
        ("response_types_supported", json!(["token"])),
        ("subject_types_supported", json!([])),
        ("code_challenge_methods_supported", json!(["plain"])),
        ("id_token_signing_alg_values_supported", json!(["HS256"])),
        (
            "authorization_endpoint",
            json!("http://identity.example/authorize"),
        ),
        ("token_endpoint", json!("https://unapproved.example/token")),
        ("jwks_uri", json!("https://unapproved.example/keys")),
        ("jwks_uri", json!(false)),
    ] {
        assert!(
            discovery_with(|metadata| metadata[field] = value).is_err(),
            "accepted {field}"
        );
    }
    assert!(
        discovery_with(|metadata| {
            metadata.as_object_mut().unwrap().remove("issuer");
        })
        .is_err()
    );
}

#[test]
fn discovery_allows_omitted_pkce_metadata_and_ignores_unselected_algorithms() {
    assert!(
        discovery_with(|metadata| {
            metadata
                .as_object_mut()
                .unwrap()
                .remove("code_challenge_methods_supported");
            metadata["id_token_signing_alg_values_supported"] =
                json!(["unknown-future-algorithm", "EdDSA"]);
        })
        .is_ok()
    );
}
