//! Applies OIDC claim policy after library-backed JWS signature verification.

use super::{Oidc, TokenInput, Verified};
use anyhow::{Result, anyhow, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{
    Algorithm, DecodingKey, Header, Validation,
    jwk::{AlgorithmParameters, EllipticCurve, Jwk, JwkSet, KeyOperations, PublicKeyUse},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256, Sha512};
use subtle::ConstantTimeEq;

#[derive(Deserialize, Serialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

impl Audience {
    fn values(&self) -> &[String] {
        match self {
            Self::One(value) => std::slice::from_ref(value),
            Self::Many(values) => values,
        }
    }
}

#[derive(Deserialize, Serialize)]
struct Claims {
    iss: String,
    sub: String,
    aud: Audience,
    iat: i64,
    exp: i64,
    nonce: String,
    #[serde(
        default,
        deserialize_with = "present_claim",
        skip_serializing_if = "Option::is_none"
    )]
    azp: Option<String>,
    #[serde(
        default,
        deserialize_with = "present_claim",
        skip_serializing_if = "Option::is_none"
    )]
    at_hash: Option<String>,
    #[serde(
        default,
        deserialize_with = "present_claim",
        skip_serializing_if = "Option::is_none"
    )]
    nbf: Option<i64>,
    #[serde(flatten)]
    other: Map<String, Value>,
}

fn present_claim<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    // Omission is optional; a present claim must still have its declared JSON type.
    T::deserialize(deserializer).map(Some)
}

pub(super) fn header(encoded: &str) -> Result<Header> {
    ensure!(encoded.len() <= 65_536, "OIDC token exceeds the limit");
    let header =
        jsonwebtoken::decode_header(encoded).map_err(|_| anyhow!("invalid OIDC token header"))?;
    ensure!(
        matches!(
            header.alg,
            Algorithm::RS256 | Algorithm::PS256 | Algorithm::ES256 | Algorithm::EdDSA
        ),
        "invalid OIDC signing algorithm"
    );
    ensure!(
        header.crit.is_none()
            && header.enc.is_none()
            && header.zip.is_none()
            && !header.extras.inner().contains_key("b64"),
        "unsupported OIDC token header"
    );
    ensure!(
        header.kid.as_ref().is_none_or(|id| !id.is_empty()
            && id.len() <= 256
            && !id.chars().any(char::is_control)),
        "invalid OIDC key identifier"
    );
    Ok(header)
}

pub(super) fn matching_key<'a>(keys: &'a JwkSet, header: &Header) -> Result<Option<&'a Jwk>> {
    ensure!(keys.keys.len() <= 32, "OIDC key count is invalid");
    let mut matching = keys.keys.iter().filter(|key| key_matches(key, header));
    let selected = matching.next();
    ensure!(matching.next().is_none(), "ambiguous OIDC verification key");
    Ok(selected)
}

fn key_matches(key: &Jwk, header: &Header) -> bool {
    let common = &key.common;
    if header
        .kid
        .as_ref()
        .is_some_and(|id| common.key_id.as_ref() != Some(id))
        || common
            .key_algorithm
            .is_some_and(|algorithm| Algorithm::try_from(algorithm).ok() != Some(header.alg))
        || common
            .public_key_use
            .as_ref()
            .is_some_and(|usage| *usage != PublicKeyUse::Signature)
        || common
            .key_operations
            .as_ref()
            .is_some_and(|operations| !operations.contains(&KeyOperations::Verify))
    {
        return false;
    }
    match (&key.algorithm, header.alg) {
        (AlgorithmParameters::RSA(_), Algorithm::RS256 | Algorithm::PS256) => true,
        (AlgorithmParameters::EllipticCurve(parameters), Algorithm::ES256) => {
            parameters.curve == EllipticCurve::P256
        }
        (AlgorithmParameters::OctetKeyPair(parameters), Algorithm::EdDSA) => {
            parameters.curve == EllipticCurve::Ed25519
        }
        _ => false,
    }
}

pub(super) fn verify_token(
    config: &Oidc,
    keys: &JwkSet,
    input: TokenInput<'_>,
) -> Result<Verified> {
    let header = header(input.encoded)?;
    let key = matching_key(keys, &header)?
        .ok_or_else(|| anyhow!("OIDC verification key is unavailable"))?;
    let key = DecodingKey::from_jwk(key).map_err(|_| anyhow!("invalid OIDC verification key"))?;
    let mut validation = Validation::new(header.alg);
    validation.set_required_spec_claims(&["iss", "sub", "aud", "exp"]);
    validation.set_issuer(&[&config.issuer]);
    validation.set_audience(&[&config.client_id]);
    // Use the injected post-response clock for both production and deterministic boundary tests.
    validation.validate_exp = false;
    let claims = jsonwebtoken::decode::<Claims>(input.encoded, &key, &validation)
        .map_err(|_| anyhow!("OIDC token verification failed"))?
        .claims;
    verify_claims(config, &claims, &input)?;
    verify_access_hash(
        header.alg,
        input.access_token.secret(),
        claims.at_hash.as_deref(),
    )?;
    let value = serde_json::to_value(&claims).map_err(|_| anyhow!("invalid OIDC claims"))?;
    ensure!(
        serde_json::to_vec(&value)?.len() <= 16_384,
        "OIDC claims exceed the limit"
    );
    Ok(Verified {
        subject: claims.sub,
        claims: value,
    })
}

fn verify_claims(config: &Oidc, claims: &Claims, input: &TokenInput<'_>) -> Result<()> {
    ensure!(claims.iss == config.issuer, "OIDC issuer mismatch");
    let audiences = claims.aud.values();
    // ponytail: Only the configured audience is trusted; extra audiences need no policy surface.
    ensure!(
        !audiences.is_empty()
            && audiences
                .iter()
                .all(|audience| audience == &config.client_id),
        "OIDC audience mismatch"
    );
    ensure!(
        claims
            .azp
            .as_ref()
            .is_none_or(|party| party == &config.client_id)
            && (audiences.len() == 1 || claims.azp.is_some()),
        "OIDC authorized party mismatch"
    );
    ensure!(
        bool::from(
            claims
                .nonce
                .as_bytes()
                .ct_eq(input.nonce.secret().as_bytes())
        ),
        "OIDC nonce mismatch"
    );
    verify_times(claims, input.now_ms)?;
    ensure!(
        !claims.sub.is_empty()
            && claims.sub.len() <= 256
            && !claims.sub.chars().any(char::is_control),
        "invalid OIDC subject"
    );
    Ok(())
}

fn verify_times(claims: &Claims, now_ms: i64) -> Result<()> {
    let issued_ms = claims
        .iat
        .checked_mul(1_000)
        .ok_or_else(|| anyhow!("invalid OIDC issue time"))?;
    let expiry_ms = claims
        .exp
        .checked_mul(1_000)
        .ok_or_else(|| anyhow!("invalid OIDC expiry time"))?;
    ensure!(
        now_ms >= 0 && claims.iat >= 0 && expiry_ms > now_ms && expiry_ms > issued_ms,
        "invalid OIDC token lifetime"
    );
    ensure!(
        (-60_000..=300_000).contains(&now_ms.saturating_sub(issued_ms)),
        "OIDC issue time is outside the login window"
    );
    if let Some(not_before) = claims.nbf {
        ensure!(
            not_before >= 0
                && not_before
                    .checked_mul(1_000)
                    .is_some_and(|time| time <= now_ms.saturating_add(60_000)),
            "OIDC token is not yet valid"
        );
    }
    Ok(())
}

fn verify_access_hash(
    algorithm: Algorithm,
    access_token: &str,
    expected: Option<&str>,
) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let actual = match algorithm {
        Algorithm::RS256 | Algorithm::PS256 | Algorithm::ES256 => {
            URL_SAFE_NO_PAD.encode(&Sha256::digest(access_token.as_bytes())[..16])
        }
        Algorithm::EdDSA => URL_SAFE_NO_PAD.encode(&Sha512::digest(access_token.as_bytes())[..32]),
        _ => return Err(anyhow!("invalid OIDC access token hash algorithm")),
    };
    ensure!(
        bool::from(expected.as_bytes().ct_eq(actual.as_bytes())),
        "OIDC access token hash mismatch"
    );
    Ok(())
}

#[cfg(test)]
#[path = "oidc_optional_claim_tests.rs"]
mod optional_claim_tests;
