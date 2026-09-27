//! Validates external identity policy before authentication or configuration mutation.

use super::{AccessRole, CameraAccess};
use anyhow::{Context, ensure};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, fmt};
use url::Url;

// These limits bound policy evaluation independently of the provider's claim limits.
const PROVIDER_LIMIT: usize = 4;
const MAPPING_LIMIT: usize = 128;
const CLAIM_BYTES_LIMIT: usize = 16 * 1024;
const CLAIM_VALUES_LIMIT: usize = 128;
const VALUE_BYTES_LIMIT: usize = 256;
const NAME_BYTES_LIMIT: usize = 64;
const URL_BYTES_LIMIT: usize = 2_048;

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub allowed_origins: Vec<String>,
    pub providers: Vec<Provider>,
    #[serde(default)]
    pub bearer_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer_transition_until_ms: Option<i64>,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExternalAuthConfig")
            .field("provider_count", &self.providers.len())
            .field("bearer_enabled", &self.bearer_enabled)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub mappings: Vec<Mapping>,
    pub method: Method,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Method {
    Oidc(Oidc),
    Proxy(Proxy),
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Oidc {
    pub issuer: String,
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    pub redirect_uri: String,
    #[serde(default = "default_scopes")]
    pub scopes: Vec<String>,
    #[serde(default = "default_display_claim")]
    pub display_name_claim: String,
    #[serde(default)]
    pub endpoint_origins: Vec<String>,
    #[serde(default)]
    pub private_networks: Vec<IpNet>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logout_uri: Option<String>,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Proxy {
    pub trusted_peers: Vec<IpNet>,
    pub subject_header: String,
    pub role_header: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_header: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_header: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_secret: Option<String>,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Mapping {
    pub claim: String,
    pub value: String,
    pub role: AccessRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_access: Option<CameraAccess>,
}

#[derive(Clone, Debug)]
pub struct Grant {
    pub role: AccessRole,
    pub camera_access: CameraAccess,
}

impl Config {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            !self.providers.is_empty() && self.providers.len() <= PROVIDER_LIMIT,
            "external authentication requires 1 to 4 providers"
        );
        validate_origins(&self.allowed_origins, true)?;
        ensure!(
            self.bearer_enabled == self.bearer_transition_until_ms.is_some(),
            "mixed authentication requires an explicit transition deadline"
        );
        ensure!(
            self.bearer_transition_until_ms
                .is_none_or(|deadline| deadline > 0),
            "authentication transition deadline must be a positive Unix millisecond timestamp"
        );
        let mut ids = HashSet::with_capacity(self.providers.len());
        let mut mapping_count = 0;
        for provider in &self.providers {
            validate_text(&provider.id, NAME_BYTES_LIMIT)?;
            ensure!(
                provider
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
                "provider ID must contain only ASCII letters, digits, hyphens, or underscores"
            );
            ensure!(ids.insert(&provider.id), "provider IDs must be unique");
            validate_text(&provider.name, NAME_BYTES_LIMIT)?;
            mapping_count += provider.mappings.len();
            ensure!(
                mapping_count <= MAPPING_LIMIT,
                "too many external role mappings"
            );
            ensure!(
                !provider.mappings.is_empty(),
                "provider requires explicit role mappings"
            );
            for mapping in &provider.mappings {
                mapping.validate()?;
            }
            match &provider.method {
                Method::Oidc(oidc) => oidc.validate(&self.allowed_origins)?,
                Method::Proxy(proxy) => proxy.validate()?,
            }
        }
        self.validate_proxy_peers()
    }

    fn validate_proxy_peers(&self) -> anyhow::Result<()> {
        // ponytail: At most four providers and 64 peer networks bound this comparison.
        for (index, provider) in self.providers.iter().enumerate() {
            let Method::Proxy(first) = &provider.method else {
                continue;
            };
            for other in &self.providers[index + 1..] {
                let Method::Proxy(second) = &other.method else {
                    continue;
                };
                ensure!(
                    !first.trusted_peers.iter().any(|first| second
                        .trusted_peers
                        .iter()
                        .any(|second| first.contains(&second.network())
                            || second.contains(&first.network()))),
                    "identity proxy providers must have disjoint trusted peer networks"
                );
            }
        }
        Ok(())
    }
}

impl Mapping {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        validate_text(&self.claim, NAME_BYTES_LIMIT)?;
        validate_text(&self.value, VALUE_BYTES_LIMIT)?;
        match (self.role, &self.camera_access) {
            (AccessRole::User, Some(policy)) => validate_camera_access(policy),
            (AccessRole::Administrator, None) => Ok(()),
            _ => anyhow::bail!(
                "Users require an explicit camera policy; Administrators cannot have one"
            ),
        }
    }
}

pub(super) fn validate_camera_access(policy: &CameraAccess) -> anyhow::Result<()> {
    policy.validate()?;
    ensure!(
        policy.camera_ids.len() + policy.group_ids.len() <= super::MAX_CAMERA_ACCESS_IDS,
        "external identity camera policy exceeds 128 combined camera and group IDs"
    );
    Ok(())
}

pub fn map_claims(rules: &[Mapping], claims: &serde_json::Value) -> anyhow::Result<Grant> {
    ensure!(
        rules.len() <= MAPPING_LIMIT,
        "too many external role mappings"
    );
    ensure!(
        serde_json::to_vec(claims)?.len() <= CLAIM_BYTES_LIMIT,
        "claims exceed size limit"
    );
    let claims = claims.as_object().context("claims must be an object")?;
    let mut matched = None;
    // ponytail: Scan at most 128 rules; index claims only if profiling warrants it.
    for rule in rules {
        rule.validate()?;
        let Some(value) = claims.get(&rule.claim) else {
            continue;
        };
        let matches = match value {
            serde_json::Value::String(value) => {
                validate_text(value, VALUE_BYTES_LIMIT)?;
                value == &rule.value
            }
            serde_json::Value::Array(values) => {
                ensure!(
                    values.len() <= CLAIM_VALUES_LIMIT,
                    "claim list exceeds size limit"
                );
                let mut matches = false;
                for value in values {
                    let value = value.as_str().context("claim list must contain strings")?;
                    validate_text(value, VALUE_BYTES_LIMIT)?;
                    matches |= value == rule.value;
                }
                matches
            }
            _ => anyhow::bail!("mapping claim must be a string or string list"),
        };
        if matches {
            ensure!(
                matched.is_none(),
                "external identity matches multiple role mappings"
            );
            matched = Some(rule);
        }
    }
    let rule = matched.context("external identity has no role mapping")?;
    Ok(Grant {
        role: rule.role,
        camera_access: rule
            .camera_access
            .clone()
            .unwrap_or_else(CameraAccess::unrestricted),
    })
}

pub fn subject_fingerprint(namespace: &str, subject: &str) -> String {
    let mut digest = Sha256::new();
    for value in [namespace, subject] {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value.as_bytes());
    }
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn admission_policy_fingerprint(provider: &Provider) -> anyhow::Result<String> {
    Ok(Sha256::digest(serde_json::to_vec(provider)?)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn validate_text(value: &str, limit: usize) -> anyhow::Result<()> {
    ensure!(
        !value.trim().is_empty() && value.len() <= limit && !value.chars().any(char::is_control),
        "external authentication text is empty, too long, or contains control characters"
    );
    Ok(())
}

fn https_url(value: &str) -> anyhow::Result<Url> {
    validate_text(value, URL_BYTES_LIMIT)?;
    let url = Url::parse(value).context("invalid external authentication URL")?;
    ensure!(
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "external authentication URL requires HTTPS without credentials or fragments"
    );
    Ok(url)
}

fn validate_origins(origins: &[String], required: bool) -> anyhow::Result<()> {
    ensure!(
        origins.len() <= 16 && (!required || !origins.is_empty()),
        "invalid origin count"
    );
    let mut seen = HashSet::with_capacity(origins.len());
    for origin in origins {
        let url = https_url(origin)?;
        ensure!(
            url.origin().ascii_serialization() == *origin,
            "origin must be canonical and exact"
        );
        ensure!(seen.insert(origin), "origins must be unique");
    }
    Ok(())
}

impl Oidc {
    fn validate(&self, origins: &[String]) -> anyhow::Result<()> {
        let issuer = https_url(&self.issuer)?;
        ensure!(issuer.query().is_none(), "issuer must not contain a query");
        validate_text(&self.client_id, VALUE_BYTES_LIMIT)?;
        if let Some(secret) = &self.client_secret {
            validate_text(secret, 4_096)?;
        }
        let callback = https_url(&self.redirect_uri)?;
        ensure!(
            callback.path() == "/auth/callback"
                && callback.query().is_none()
                && origins.contains(&callback.origin().ascii_serialization()),
            "callback must use an allowed origin and the exact /auth/callback path"
        );
        ensure!(
            self.scopes.len() <= 16 && self.scopes.iter().any(|s| s == "openid"),
            "invalid OIDC scopes"
        );
        let mut scopes = HashSet::with_capacity(self.scopes.len());
        for scope in &self.scopes {
            validate_text(scope, NAME_BYTES_LIMIT)?;
            ensure!(
                scope
                    .bytes()
                    .all(|b| b == 0x21 || (0x23..=0x5b).contains(&b) || (0x5d..=0x7e).contains(&b))
                    && scopes.insert(scope),
                "OIDC scopes must be unique OAuth scope tokens"
            );
        }
        validate_text(&self.display_name_claim, NAME_BYTES_LIMIT)?;
        validate_origins(&self.endpoint_origins, false)?;
        ensure!(
            self.private_networks.len() <= 64,
            "too many private issuer networks"
        );
        if let Some(logout) = &self.logout_uri {
            let logout = https_url(logout)?;
            ensure!(
                logout.query().is_none(),
                "logout endpoint must not contain a query"
            );
        }
        Ok(())
    }
}

impl Proxy {
    fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            !self.trusted_peers.is_empty() && self.trusted_peers.len() <= 64,
            "invalid identity proxy peer count"
        );
        ensure!(
            self.secret_header.is_some() == self.shared_secret.is_some(),
            "proxy secret requires both header and value"
        );
        let mut names = HashSet::with_capacity(4);
        for name in [
            Some(&self.subject_header),
            Some(&self.role_header),
            self.name_header.as_ref(),
            self.secret_header.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            validate_text(name, NAME_BYTES_LIMIT)?;
            ensure!(
                name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
                "invalid identity header name"
            );
            let name = name.to_ascii_lowercase();
            ensure!(
                name.starts_with("x-") && !name.starts_with("x-forwarded-") && names.insert(name),
                "identity headers must be distinct non-forwarding X- headers"
            );
        }
        if let Some(secret) = &self.shared_secret {
            validate_text(secret, 4_096)?;
        }
        Ok(())
    }
}

fn default_scopes() -> Vec<String> {
    vec!["openid".into()]
}
fn default_display_claim() -> String {
    "name".into()
}

pub fn validate_source(root: &toml::Table) -> anyhow::Result<()> {
    let Some(value) = root.get("external_auth") else {
        return Ok(());
    };
    let config: Config = value
        .clone()
        .try_into()
        .map_err(|_| anyhow::anyhow!("external authentication configuration has invalid fields"))?;
    for provider in &config.providers {
        let secret = match &provider.method {
            Method::Oidc(oidc) => oidc.client_secret.as_deref(),
            Method::Proxy(proxy) => proxy.shared_secret.as_deref(),
        };
        if let Some(secret) = secret {
            ensure!(
                crate::config::is_secret_reference(secret),
                "external authentication secrets must use complete secret references"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "external_tests.rs"]
mod tests;
