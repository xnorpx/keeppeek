use super::{TokenInput, verify_token};
use crate::access::external::Oidc;
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use jsonwebtoken::{
    Algorithm, EncodingKey, Header,
    jwk::{Jwk, JwkSet},
};
use oauth2::{AccessToken, CsrfToken as Nonce};
use sha2::{Digest, Sha512};

// Public test-only key from openidconnect's Ed25519 verification fixture.
const SIGNING_KEY_DER: &str = "MC4CAQAwBQYDK2VwBCIEICWeYPLxoZKHZlQ6rkBi11E9JwchynXtljATLqym/XS9";

// Public test-only PKCS#1 DER from jsonwebtoken 11.1.0, tests/rsa/private_rsa_key.der.
// Upstream: github.com/Keats/jsonwebtoken; MIT, Copyright (c) 2015 Vincent Prouillet.
const RSA_SIGNING_KEY_DER: &str = concat!(
    "MIIEpAIBAAKCAQEAyRE6rHuNR0QbHO3H3Kt2pOKGVhQqGZXInOduQNxXzuKlvQTLUTv4l4sggh5/CYYi",
    "/cvI+SXVT9kPWSKXxJXBXd/4LkvcPuUakBoAkfh+eiFVMh2VrUyWyj3MFl0HTVF9KwRXLAcwkREiS3np",
    "ThHRyIxuy0ZMeZfxVL5arMhw1SRELB8HoGfG/AtH89BIE9jDBHZ9dLelK9a184zAf8LwoPLxvJb3Il5n",
    "ncqPcSfKDDodMFBIMc4lQzDKL5gvmiXLXB1AGLm8KBjfE8s3L5xqi+yUod+j8MtvIj812dkS4QMiRVN/",
    "by2h3ZY8LYVGrqZXZTcgn2ujn8uKjXLZVD5TdQIDAQABAoIBAHREk0I0O9DvECKdWUpAmF3mY7oY9PNQ",
    "iu44Yaf+AoSuyRpRUGTMIgc3u3eivOE8ALX0BmYUO5JtuRNZDpvt4SAwqCnVUinIf6C+eH/wSurCpapS",
    "M0BAHp4aOA7igptyOMgMPYBHNA1e9A7jE0dCxKWMl3DSWNyjQTk4zeRGEAEfbNjHrq6YCtjHSZSLmWiG",
    "80hnfnYos9hOr5JnLnyS7ZmFE/5P3XVrxLc/tQ5zum0R4cbrgzHiQP5RgfxGJaEi7XcgherCCOgurJSS",
    "bYH29Gz8u5fFbS+Yg8s+OiCss3cs1rSgJ9/eHZuzGEdUZVARH6hVMjSuwvqVTFaE8AgtleECgYEA+uLM",
    "n4kNqHlJS2A5uAnCkj90ZxEtNm3E8hAxUrhssktY5XSOAPBlxyf5RuRGIImGtUVIr4HuJSa5TX48n3Vd",
    "t9MYCprO/iYl6moNRSPt5qowIIOJmIjY2mqPDfDt/zw+fcDD3lmCJrFlzcnh0uea1CohxEbQnL3cypeL",
    "t+WbU6kCgYEAzSp19m1ajieFkqgoB0YTpt/OroDx38vvI5unInJlEeOjQ+oIAQdN2wpxBvTrRorMU6P0",
    "7mFUbt1j+Co6CbNiw+X8HcCaqYLR5clbJOOWNR36PuzOpQLkfK8woupBxzW9B8gZmY8rB1mbJ+/WTPrE",
    "Jy6YGmIEBkWylQ2VpW8O4O0CgYEApdbvvfFBlwD9YxbrcGz7MeNCFbMz+MucqQntIKoKJ91ImPxvtc0y",
    "6e/Rhnv0oyNlaUOwJVu0yNgNG117w0g4t/+Q38mvVC5xV7/cn7x9UMFk6MkqVir3dYGEqIl/OP1grY2T",
    "q9HtB5iyG9L8NIamQOLMyUqqMUILxdthHyFmiGkCgYEAn9+PjpjGMPHxL0gj8Q8VbzsFtou6b1deIRRA",
    "2CHmSltltR1gYVTMwXxQeUhPMmgkMqUXzs4/WijgpthY44hK1TaZEKIuoxrS70nJ4WQLf5a9k1065fDs",
    "FZD6yGjdGxvwEmlGMZgTwqV7t1I4X0Ilqhav5hcs5apYL7gnPYPeRz0CgYALHCj/Ji8XSsDoF/MhVhnG",
    "dIs2P99NNdmo3R2Pv0CuZbDKMU559LJHUvrKS8WkuWRDuKrz1W/EQKApFjDGpdqToZqriUFQzwy7mR3a",
    "yIiogzNtHcvbDHx8oFnGY0OFksX/ye0/XGpy2SFxYRwGU98HPYeBvAQQrVjdkzfy7BmXQQ==",
);

fn settings() -> Oidc {
    toml::from_str(
        r#"issuer = "https://identity.example"
client_id = "keeppeek"
redirect_uri = "https://keeppeek.example/auth/callback"
"#,
    )
    .unwrap()
}

pub(super) fn claims() -> serde_json::Value {
    serde_json::json!({
        "iss": "https://identity.example", "aud": "keeppeek", "sub": "alice",
        "iat": 1_800_000_000, "exp": 1_800_000_120, "nonce": "synthetic-nonce",
        "groups": ["viewers"], "name": "Alice",
        "at_hash": URL_SAFE_NO_PAD.encode(&Sha512::digest(b"synthetic-access-token")[..32])
    })
}

pub(super) fn sign(claims: serde_json::Value) -> (String, JwkSet) {
    sign_generation(claims, 0)
}

pub(super) fn sign_generation(claims: serde_json::Value, generation: u8) -> (String, JwkSet) {
    let (key, jwk) = signing_key(generation);
    let mut header = Header::new(Algorithm::EdDSA);
    header.kid = jwk.common.key_id.clone();
    let token = jsonwebtoken::encode(&header, &claims, &key).unwrap();
    (token, JwkSet { keys: vec![jwk] })
}

fn signing_key(generation: u8) -> (EncodingKey, Jwk) {
    let mut bytes = STANDARD.decode(SIGNING_KEY_DER).unwrap();
    *bytes.last_mut().unwrap() ^= generation;
    let key = EncodingKey::from_ed_der(&bytes);
    let mut jwk = Jwk::from_encoding_key(&key, Algorithm::EdDSA).unwrap();
    jwk.common.key_id = Some(format!("fixture-{generation}"));
    (key, jwk)
}

fn verify(encoded: &str, keys: &JwkSet) -> anyhow::Result<super::Verified> {
    verify_token(
        &settings(),
        keys,
        TokenInput {
            encoded,
            nonce: &Nonce::new("synthetic-nonce".into()),
            access_token: &AccessToken::new("synthetic-access-token".into()),
            now_ms: 1_800_000_010_000,
        },
    )
}

#[test]
fn oidc_validates_signed_identity_and_preserves_mapping_claims() {
    let (token, keys) = sign(claims());
    let verified = verify(&token, &keys).unwrap();
    assert_eq!(verified.subject, "alice");
    assert_eq!(verified.claims["groups"], serde_json::json!(["viewers"]));
    assert!(!format!("{verified:?}").contains("alice"));
}

#[test]
fn oidc_rejects_wrong_issuer_audience_nonce_time_and_authorized_party() {
    for (field, value) in [
        ("iss", serde_json::json!("https://other.example")),
        ("aud", serde_json::json!("another-client")),
        ("nonce", serde_json::json!("wrong-nonce")),
        ("exp", serde_json::json!(1_800_000_000)),
        ("iat", serde_json::json!(1_800_000_100)),
        ("iat", serde_json::json!(1_799_999_600)),
        ("azp", serde_json::json!("another-client")),
        ("aud", serde_json::json!(["keeppeek", "other"])),
    ] {
        let mut changed = claims();
        changed[field] = value;
        let (token, keys) = sign(changed);
        assert!(verify(&token, &keys).is_err(), "accepted invalid {field}");
    }
}

#[test]
fn oidc_rejects_unknown_keys_oversized_and_unsigned_tokens() {
    let (token, keys) = sign(claims());
    assert!(verify(&token, &JwkSet { keys: vec![] }).is_err());
    assert!(verify(&"x".repeat(65_537), &keys).is_err());
    let unsigned = format!(
        "{}.{}.",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims()).unwrap())
    );
    assert!(verify(&unsigned, &keys).is_err());
    let mut changed = claims();
    changed["name"] = serde_json::json!("x".repeat(16_385));
    let (token, keys) = sign(changed);
    assert!(verify(&token, &keys).is_err());
}

#[test]
fn oidc_rejects_keys_without_verification_permission_or_matching_curve() {
    let (token, keys) = sign(claims());
    for (field, value) in [
        ("use", serde_json::json!("enc")),
        ("use", serde_json::json!("unknown-use")),
        ("key_ops", serde_json::json!([])),
        ("key_ops", serde_json::json!(["sign"])),
        ("key_ops", serde_json::json!(["encrypt"])),
        ("crv", serde_json::json!("P-256")),
        ("alg", serde_json::json!("ES256")),
    ] {
        let mut changed = serde_json::to_value(&keys).unwrap();
        changed["keys"][0][field] = value;
        let changed: JwkSet = serde_json::from_value(changed).unwrap();
        assert!(
            verify(&token, &changed).is_err(),
            "accepted invalid key {field}"
        );
    }
    let mut permitted = serde_json::to_value(keys).unwrap();
    permitted["keys"][0]["use"] = "sig".into();
    permitted["keys"][0]["key_ops"] = serde_json::json!(["verify"]);
    let permitted: JwkSet = serde_json::from_value(permitted).unwrap();
    assert!(verify(&token, &permitted).is_ok());
}

#[test]
fn oidc_rejects_unsupported_critical_headers_even_with_a_valid_signature() {
    let (key, jwk) = signing_key(0);
    for extension in ["unknown-extension", "b64"] {
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = jwk.common.key_id.clone();
        header.crit = Some(vec![extension.into()]);
        header.extras.insert(extension, true);
        let token = jsonwebtoken::encode(&header, &claims(), &key).unwrap();
        assert!(
            verify(
                &token,
                &JwkSet {
                    keys: vec![jwk.clone()]
                }
            )
            .is_err()
        );
    }
}

#[test]
fn oidc_rejects_malformed_required_claims_and_numeric_dates() {
    for (field, value) in [
        ("sub", serde_json::json!(null)),
        ("sub", serde_json::json!(42)),
        ("sub", serde_json::json!("")),
        ("sub", serde_json::json!("alice\n")),
        ("sub", serde_json::json!("a".repeat(257))),
        ("iss", serde_json::json!([])),
        ("aud", serde_json::json!([])),
        ("aud", serde_json::json!(["keeppeek", 42])),
        ("nonce", serde_json::json!(null)),
        ("nonce", serde_json::json!({"value": "synthetic-nonce"})),
        ("iat", serde_json::json!("1800000000")),
        ("iat", serde_json::json!(1_800_000_000.5)),
        ("iat", serde_json::json!(null)),
        ("iat", serde_json::json!(u64::MAX)),
        ("exp", serde_json::json!("1800000120")),
        ("exp", serde_json::json!(1_800_000_120.5)),
        ("exp", serde_json::json!(null)),
        ("exp", serde_json::json!(u64::MAX)),
    ] {
        let mut changed = claims();
        changed[field] = value;
        let (token, keys) = sign(changed);
        assert!(verify(&token, &keys).is_err(), "accepted malformed {field}");
    }
    for field in ["iss", "aud", "sub", "nonce", "iat", "exp"] {
        let mut changed = claims();
        changed.as_object_mut().unwrap().remove(field);
        let (token, keys) = sign(changed);
        assert!(verify(&token, &keys).is_err(), "accepted missing {field}");
    }
    for value in [serde_json::json!([]), serde_json::json!(null)] {
        let (token, keys) = sign(value);
        assert!(verify(&token, &keys).is_err());
    }
}

#[test]
fn oidc_enforces_exact_expiry_lifetime_and_issue_time_boundaries() {
    for (issued, expires, accepted) in [
        (1_799_999_710, 1_800_000_120, true),
        (1_799_999_709, 1_800_000_120, false),
        (1_800_000_070, 1_800_000_120, true),
        (1_800_000_071, 1_800_000_120, false),
        (1_800_000_000, 1_800_000_010, false),
        (1_800_000_000, 1_800_000_011, true),
        (1_800_000_020, 1_800_000_020, false),
        (1_800_000_021, 1_800_000_020, false),
    ] {
        let mut changed = claims();
        changed["iat"] = issued.into();
        changed["exp"] = expires.into();
        let (token, keys) = sign(changed);
        assert_eq!(
            verify(&token, &keys).is_ok(),
            accepted,
            "iat={issued} exp={expires}"
        );
    }
}

#[test]
fn oidc_validates_optional_access_token_hash() {
    let mut changed = claims();
    changed.as_object_mut().unwrap().remove("at_hash");
    changed["azp"] = "keeppeek".into();
    let (token, keys) = sign(changed);
    assert!(verify(&token, &keys).is_ok());
    for value in [
        serde_json::json!(""),
        serde_json::json!("not-base64!"),
        serde_json::json!(URL_SAFE_NO_PAD.encode([0_u8; 32])),
        serde_json::json!(
            URL_SAFE_NO_PAD.encode(&sha2::Sha256::digest(b"synthetic-access-token")[..16])
        ),
        serde_json::json!(42),
        serde_json::json!([]),
    ] {
        let mut changed = claims();
        changed["at_hash"] = value;
        let (token, keys) = sign(changed);
        assert!(verify(&token, &keys).is_err());
    }
    let (token, keys) = sign(claims());
    assert!(
        verify_token(
            &settings(),
            &keys,
            TokenInput {
                encoded: &token,
                nonce: &Nonce::new("synthetic-nonce".into()),
                access_token: &AccessToken::new("different-access-token".into()),
                now_ms: 1_800_000_010_000,
            }
        )
        .is_err()
    );
}

#[test]
fn oidc_rejects_hmac_algorithm_confusion_using_public_key_bytes() {
    let (_, jwk) = signing_key(0);
    let value = serde_json::to_value(&jwk).unwrap();
    let public = URL_SAFE_NO_PAD
        .decode(value["x"].as_str().unwrap())
        .unwrap();
    let key = EncodingKey::from_secret(&public);
    let mut header = Header::new(Algorithm::HS256);
    header.kid = jwk.common.key_id.clone();
    let mut payload = claims();
    payload.as_object_mut().unwrap().remove("at_hash");
    let token = jsonwebtoken::encode(&header, &payload, &key).unwrap();
    assert!(verify(&token, &JwkSet { keys: vec![jwk] }).is_err());
    let mut symmetric = Jwk::from_encoding_key(&key, Algorithm::HS256).unwrap();
    symmetric.common.key_id = header.kid;
    assert!(
        verify(
            &token,
            &JwkSet {
                keys: vec![symmetric]
            }
        )
        .is_err()
    );
}

#[test]
fn oidc_rejects_corrupted_signature_without_exposing_claims() {
    let (token, keys) = sign(claims());
    let (payload, signature) = token.rsplit_once('.').unwrap();
    let mut signature = URL_SAFE_NO_PAD.decode(signature).unwrap();
    signature[0] ^= 1;
    let tampered = format!("{payload}.{}", URL_SAFE_NO_PAD.encode(signature));
    let error = verify(&tampered, &keys).expect_err("invalid signature accepted");
    assert!(!format!("{error:?}").contains("alice"));
    assert!(!format!("{error:?}").contains(&tampered));
}

#[test]
fn oidc_verifies_real_es256_rs256_and_ps256_signatures_and_access_hashes() {
    let ec = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
    let ec = EncodingKey::from_ec_der(&ec.serialize_der());
    let rsa = EncodingKey::from_rsa_der(&STANDARD.decode(RSA_SIGNING_KEY_DER).unwrap());
    for (algorithm, key) in [
        (Algorithm::ES256, &ec),
        (Algorithm::RS256, &rsa),
        (Algorithm::PS256, &rsa),
    ] {
        let mut jwk = Jwk::from_encoding_key(key, algorithm).unwrap();
        jwk.common.key_id = Some("asymmetric-fixture".into());
        let mut header = Header::new(algorithm);
        header.kid = jwk.common.key_id.clone();
        let keys = JwkSet { keys: vec![jwk] };
        let mut payload = claims();
        payload["at_hash"] = URL_SAFE_NO_PAD
            .encode(&sha2::Sha256::digest(b"synthetic-access-token")[..16])
            .into();
        let token = jsonwebtoken::encode(&header, &payload, key).unwrap();
        assert_eq!(
            verify(&token, &keys).unwrap().subject,
            "alice",
            "{algorithm:?}"
        );
        payload["at_hash"] = URL_SAFE_NO_PAD.encode([0_u8; 16]).into();
        let token = jsonwebtoken::encode(&header, &payload, key).unwrap();
        assert!(
            verify(&token, &keys).is_err(),
            "invalid hash with {algorithm:?}"
        );
    }
}

#[test]
fn oidc_rejects_untrusted_extra_audience_even_with_matching_authorized_party() {
    let mut payload = claims();
    payload["aud"] = serde_json::json!(["keeppeek", "other"]);
    payload["azp"] = "keeppeek".into();
    let (token, keys) = sign(payload);
    assert!(verify(&token, &keys).is_err());
}
