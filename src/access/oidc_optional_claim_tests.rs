use super::super::tests::{claims, sign};
use super::*;
use oauth2::{AccessToken, CsrfToken};

fn verify(claims: Value) -> Result<Verified> {
    let (encoded, keys) = sign(claims);
    let config: Oidc = toml::from_str(
        "issuer = 'https://identity.example'\nclient_id = 'keeppeek'\nredirect_uri = 'https://keeppeek.example/auth/callback'",
    ).unwrap();
    verify_token(
        &config,
        &keys,
        TokenInput {
            encoded: &encoded,
            nonce: &CsrfToken::new("synthetic-nonce".into()),
            access_token: &AccessToken::new("synthetic-access-token".into()),
            now_ms: 1_800_000_010_000,
        },
    )
}

#[test]
fn signed_optional_claims_reject_null_instead_of_skipping_validation() {
    for field in ["azp", "at_hash", "nbf"] {
        let mut claims = claims();
        claims[field] = Value::Null;
        assert!(verify(claims).is_err(), "accepted null {field}");
    }
}

#[test]
fn signed_optional_claims_allow_omission_and_valid_typed_values() {
    let mut omitted = claims();
    for field in ["azp", "at_hash", "nbf"] {
        omitted.as_object_mut().unwrap().remove(field);
    }
    assert!(verify(omitted).is_ok());
    let mut present = claims();
    present["azp"] = Value::String("keeppeek".into());
    present["nbf"] = Value::from(1_800_000_010_i64);
    assert!(verify(present).is_ok());
}
