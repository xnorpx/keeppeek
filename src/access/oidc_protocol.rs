//! Parses bounded discovery responses and exchanges authorization codes through OAuth2.

use super::{Exchange, Oidc, ProviderUnavailable};
use crate::access::oidc_transport::{Policy, Transport};
use anyhow::{Result, anyhow, ensure};
use jsonwebtoken::jwk::JwkSet;
use oauth2::{
    AuthorizationCode, ClientId, ClientSecret, RedirectUrl, StandardTokenResponse, TokenUrl,
    basic::{
        BasicErrorResponse, BasicRevocationErrorResponse, BasicTokenIntrospectionResponse,
        BasicTokenType,
    },
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::fmt;

#[derive(Deserialize)]
pub(super) struct Metadata {
    issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
    pub id_token_signing_alg_values_supported: Vec<String>,
    response_types_supported: Vec<String>,
    subject_types_supported: Vec<String>,
    code_challenge_methods_supported: Option<Vec<String>>,
    token_endpoint_auth_methods_supported: Option<Vec<String>>,
}

impl Metadata {
    pub(super) fn discover(config: &Oidc, transport: &Transport) -> Result<(Self, JwkSet)> {
        let url = format!(
            "{}/.well-known/openid-configuration",
            config.issuer.trim_end_matches('/')
        );
        let metadata: Self = fetch(transport, &url)?;
        ensure!(
            metadata.issuer == config.issuer,
            "OIDC discovery issuer mismatch"
        );
        let policy = Policy::new(config)?;
        for endpoint in [
            &metadata.authorization_endpoint,
            &metadata.token_endpoint,
            &metadata.jwks_uri,
        ] {
            policy.endpoint(endpoint)?;
        }
        ensure!(
            metadata
                .response_types_supported
                .iter()
                .any(|value| value == "code"),
            "OIDC code flow is unavailable"
        );
        ensure!(
            metadata
                .subject_types_supported
                .iter()
                .any(|value| matches!(value.as_str(), "public" | "pairwise")),
            "OIDC subject type is unavailable"
        );
        ensure!(
            metadata
                .code_challenge_methods_supported
                .as_ref()
                .is_none_or(|values| values.iter().any(|value| value == "S256")),
            "OIDC S256 is unavailable"
        );
        if config.client_secret.is_some() {
            ensure!(
                metadata
                    .token_endpoint_auth_methods_supported
                    .as_ref()
                    .is_none_or(|values| values.iter().any(|value| value == "client_secret_basic")),
                "OIDC client authentication is unavailable"
            );
        }
        ensure!(
            metadata
                .id_token_signing_alg_values_supported
                .iter()
                .any(|value| matches!(value.as_str(), "RS256" | "PS256" | "ES256" | "EdDSA")),
            "OIDC signing algorithm is unavailable"
        );
        let keys = fetch_keys(transport, &metadata.jwks_uri)?;
        Ok((metadata, keys))
    }
}

fn fetch<T: DeserializeOwned>(transport: &Transport, url: &str) -> Result<T> {
    let request = oauth2::http::Request::builder()
        .uri(url)
        .header("Accept", "application/json")
        .body(vec![])
        .map_err(|_| anyhow!("invalid OIDC discovery request"))?;
    let response = transport
        .request(request)
        .map_err(|_| anyhow!(ProviderUnavailable))?;
    ensure!(
        response.status() == oauth2::http::StatusCode::OK,
        "OIDC discovery failed"
    );
    let content_type = response
        .headers()
        .get("Content-Type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    ensure!(
        content_type
            .split(';')
            .next()
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json")),
        "invalid OIDC discovery content type"
    );
    serde_json::from_slice(response.body()).map_err(|_| anyhow!("invalid OIDC discovery response"))
}

pub(super) fn fetch_keys(transport: &Transport, url: &str) -> Result<JwkSet> {
    let keys: JwkSet = fetch(transport, url)?;
    ensure!(
        !keys.keys.is_empty() && keys.keys.len() <= 32,
        "OIDC key count is invalid"
    );
    Ok(keys)
}

#[derive(Deserialize, Serialize)]
pub(super) struct TokenFields {
    pub id_token: String,
}

impl fmt::Debug for TokenFields {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OidcTokenFields")
            .finish_non_exhaustive()
    }
}

impl oauth2::ExtraTokenFields for TokenFields {}

type TokenResponse = StandardTokenResponse<TokenFields, BasicTokenType>;
type Client = oauth2::Client<
    BasicErrorResponse,
    TokenResponse,
    BasicTokenIntrospectionResponse,
    oauth2::StandardRevocableToken,
    BasicRevocationErrorResponse,
>;

pub(super) fn exchange(
    config: &Oidc,
    endpoint: &str,
    transport: &Transport,
    input: Exchange<'_>,
) -> Result<TokenResponse> {
    let mut client = Client::new(ClientId::new(config.client_id.clone()));
    if let Some(secret) = &config.client_secret {
        client = client.set_client_secret(ClientSecret::new(secret.clone()));
    }
    let client = client
        .set_token_uri(
            TokenUrl::new(endpoint.to_owned())
                .map_err(|_| anyhow!("invalid OIDC token endpoint"))?,
        )
        .set_redirect_uri(
            RedirectUrl::new(config.redirect_uri.clone())
                .map_err(|_| anyhow!("invalid OIDC redirect URI"))?,
        );
    client
        .exchange_code(AuthorizationCode::new(input.code.to_owned()))
        .set_pkce_verifier(input.verifier)
        .request(&|request| transport.request(request))
        .map_err(|error| match error {
            oauth2::RequestTokenError::Request(_) => anyhow!(ProviderUnavailable),
            _ => anyhow!("OIDC code exchange failed"),
        })
}

#[cfg(test)]
#[path = "oidc_protocol_tests.rs"]
mod tests;
