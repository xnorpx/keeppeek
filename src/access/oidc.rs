//! Validates OIDC tokens with the provider's pinned policy and bounded protocol inputs.

use super::external::Oidc;
use anyhow::{Result, anyhow, ensure};
use jsonwebtoken::jwk::JwkSet;
use oauth2::{AccessToken, CsrfToken as Nonce, TokenResponse};
use serde_json::Value;
use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[path = "oidc_protocol.rs"]
mod protocol;
#[path = "oidc_tokens.rs"]
mod tokens;
use tokens::verify_token;

const ISSUER_BUDGET_LIMIT: usize = 8;
const DISCOVERY_COOLDOWN: Duration = Duration::from_secs(300);
const EMERGENCY_COOLDOWN: Duration = Duration::from_secs(60);

#[derive(Clone, Default)]
pub struct IssuerBudgets {
    entries: Arc<Mutex<HashMap<String, Arc<Mutex<IssuerBudget>>>>>,
}

#[derive(Default)]
struct IssuerBudget {
    last_discovery: Option<Instant>,
    last_emergency: Option<Instant>,
}

impl IssuerBudgets {
    fn reserve_discovery(&self, issuer: &str, now: Instant) -> Result<Arc<Mutex<IssuerBudget>>> {
        ensure!(issuer.len() <= 2_048, "invalid OIDC issuer");
        let issuer = url::Url::parse(issuer).map_err(|_| anyhow!("invalid OIDC issuer"))?;
        let mut entries = self
            .entries
            .try_lock()
            .map_err(|_| anyhow!("OIDC issuer budgets are busy"))?;
        // ponytail: Eight issuer records bound this scan; leases retain old provider generations.
        entries.retain(|_, budget| {
            Arc::strong_count(budget) > 1
                || budget
                    .try_lock()
                    .map_or(true, |budget| budget.cooling_down(now))
        });
        ensure!(
            entries.contains_key(issuer.as_str()) || entries.len() < ISSUER_BUDGET_LIMIT,
            "OIDC issuer budget capacity exceeded"
        );
        let lease = entries
            .entry(issuer.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(IssuerBudget::default())))
            .clone();
        {
            let mut budget = lease
                .try_lock()
                .map_err(|_| anyhow!("OIDC issuer budget is busy"))?;
            ensure!(
                elapsed(budget.last_discovery, now, DISCOVERY_COOLDOWN),
                "OIDC discovery is temporarily limited"
            );
            budget.last_discovery = Some(now);
        }
        Ok(lease)
    }
}

impl IssuerBudget {
    fn cooling_down(&self, now: Instant) -> bool {
        !elapsed(self.last_discovery, now, DISCOVERY_COOLDOWN)
            || !elapsed(self.last_emergency, now, EMERGENCY_COOLDOWN)
    }

    fn reserve_emergency(&mut self, now: Instant) -> Result<()> {
        ensure!(
            elapsed(self.last_emergency, now, EMERGENCY_COOLDOWN),
            "OIDC key refresh is temporarily limited"
        );
        self.last_emergency = Some(now);
        Ok(())
    }
}

fn elapsed(previous: Option<Instant>, now: Instant, cooldown: Duration) -> bool {
    previous.is_none_or(|previous| now.saturating_duration_since(previous) >= cooldown)
}

#[derive(Default)]
pub struct Cache {
    revision: u64,
    entries: HashMap<String, CacheEntry>,
}

struct CacheEntry {
    attempted_at: Instant,
    provider: Option<Provider>,
}

#[derive(Default)]
pub struct CandidateCache {
    entries: HashMap<[u8; 32], CandidateEntry>,
}

struct CandidateEntry {
    issuer: String,
    attempted_at: Instant,
    provider: Option<Provider>,
}

impl CandidateCache {
    pub(crate) fn get(
        &mut self,
        settings: &Oidc,
        now: Instant,
        discover: impl FnOnce() -> Result<Provider>,
    ) -> Result<Provider> {
        use sha2::{Digest, Sha256};
        let key: [u8; 32] = Sha256::digest(serde_json::to_vec(settings)?).into();
        self.entries.retain(|_, entry| {
            now.saturating_duration_since(entry.attempted_at) < Duration::from_secs(300)
                || entry
                    .provider
                    .as_ref()
                    .is_some_and(|provider| provider.retains_cache_entry(now))
        });
        if let Some(entry) = self.entries.get(&key) {
            ensure!(
                now.saturating_duration_since(entry.attempted_at) < Duration::from_secs(300),
                "OIDC candidate metadata expired while in use"
            );
            return entry
                .provider
                .clone()
                .ok_or_else(|| anyhow!("OIDC candidate is temporarily unavailable"));
        }
        ensure!(
            self.entries.len() < 4,
            "OIDC candidate cache capacity exceeded"
        );
        // ponytail: Reject policy variants until the issuer slot can be reclaimed.
        ensure!(
            !self
                .entries
                .values()
                .any(|entry| entry.issuer == settings.issuer),
            "OIDC issuer already has a candidate policy in use"
        );
        self.entries.insert(
            key,
            CandidateEntry {
                issuer: settings.issuer.clone(),
                attempted_at: now,
                provider: None,
            },
        );
        let provider = discover()?;
        ensure!(
            &provider.config == settings,
            "OIDC candidate discovery policy mismatch"
        );
        self.entries
            .get_mut(&key)
            .expect("candidate slot exists")
            .provider = Some(provider.clone());
        Ok(provider)
    }
}

impl Cache {
    pub(crate) fn get(
        &mut self,
        revision: u64,
        id: &str,
        now: Instant,
        discover: impl FnOnce() -> Result<Provider>,
    ) -> Result<Provider> {
        ensure!(
            revision > 0 && !id.is_empty() && id.len() <= 64,
            "invalid OIDC cache binding"
        );
        if self.revision != revision {
            self.entries.clear();
            self.revision = revision;
        }
        if let Some(entry) = self.entries.get(id)
            && now.saturating_duration_since(entry.attempted_at) < Duration::from_secs(300)
        {
            return entry
                .provider
                .clone()
                .ok_or_else(|| anyhow!("OIDC provider is temporarily unavailable"));
        }
        ensure!(
            self.entries.contains_key(id) || self.entries.len() < 4,
            "OIDC provider cache capacity exceeded"
        );
        self.entries.insert(
            id.to_owned(),
            CacheEntry {
                attempted_at: now,
                provider: None,
            },
        );
        let provider = discover()?;
        self.entries
            .get_mut(id)
            .expect("provider cache slot exists")
            .provider = Some(provider.clone());
        Ok(provider)
    }
}

#[derive(Clone)]
pub struct Provider {
    config: Oidc,
    metadata: Arc<protocol::Metadata>,
    transport: super::oidc_transport::Transport,
    keys: Arc<Mutex<KeyCache>>,
    budget: Arc<Mutex<IssuerBudget>>,
}

struct KeyCache {
    current: JwkSet,
}

pub struct Exchange<'a> {
    pub code: &'a str,
    pub nonce: &'a Nonce,
    pub verifier: oauth2::PkceCodeVerifier,
}

impl Provider {
    fn retains_cache_entry(&self, now: Instant) -> bool {
        // Every Provider clone shares this key cache; external clones pin its discovery policy.
        Arc::strong_count(&self.keys) > 1
            || self.budget.try_lock().map_or(true, |budget| {
                !elapsed(budget.last_emergency, now, EMERGENCY_COOLDOWN)
            })
    }

    #[cfg(test)]
    pub(crate) fn discover(
        config: &Oidc,
        transport: super::oidc_transport::Transport,
    ) -> Result<Self> {
        Self::discover_with_budget(config, transport, &IssuerBudgets::default(), Instant::now())
    }

    pub(crate) fn discover_with_budget(
        config: &Oidc,
        transport: super::oidc_transport::Transport,
        budgets: &IssuerBudgets,
        now: Instant,
    ) -> Result<Self> {
        let budget = budgets.reserve_discovery(&config.issuer, now)?;
        let (metadata, keys) = protocol::Metadata::discover(config, &transport)?;
        Ok(Self {
            config: config.clone(),
            keys: Arc::new(Mutex::new(KeyCache { current: keys })),
            metadata: Arc::new(metadata),
            transport,
            budget,
        })
    }

    pub(crate) fn authorization_endpoint(&self) -> &str {
        &self.metadata.authorization_endpoint
    }

    pub(crate) fn exchange(
        &self,
        input: Exchange<'_>,
        now_ms: impl Fn() -> i64,
    ) -> Result<Verified> {
        ensure!(
            !input.code.is_empty()
                && input.code.len() <= 4_096
                && !input.code.chars().any(char::is_control),
            "invalid OIDC authorization code"
        );
        let nonce = input.nonce;
        let response = protocol::exchange(
            &self.config,
            &self.metadata.token_endpoint,
            &self.transport,
            input,
        )?;
        let encoded = &response.extra_fields().id_token;
        let header = tokens::header(encoded)?;
        let algorithm =
            serde_json::to_value(header.alg).map_err(|_| anyhow!("invalid OIDC algorithm"))?;
        ensure!(
            self.metadata
                .id_token_signing_alg_values_supported
                .iter()
                .any(|advertised| Some(advertised.as_str()) == algorithm.as_str()),
            "OIDC algorithm was not advertised"
        );
        let keys = self.keys_for(encoded)?;
        verify_token(
            &self.config,
            &keys,
            TokenInput {
                encoded,
                nonce,
                access_token: response.access_token(),
                now_ms: now_ms(),
            },
        )
    }

    fn keys_for(&self, encoded: &str) -> Result<JwkSet> {
        let header = tokens::header(encoded)?;
        let mut cache = self
            .keys
            .try_lock()
            .map_err(|_| anyhow!("OIDC key refresh is busy"))?;
        if tokens::matching_key(&cache.current, &header)?.is_some() {
            return Ok(cache.current.clone());
        }
        self.budget
            .try_lock()
            .map_err(|_| anyhow!("OIDC issuer budget is busy"))?
            .reserve_emergency(Instant::now())?;
        let keys = protocol::fetch_keys(&self.transport, &self.metadata.jwks_uri)?;
        cache.current = keys;
        Ok(cache.current.clone())
    }
}

#[derive(Debug)]
pub struct ProviderUnavailable;

impl fmt::Display for ProviderUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OIDC provider is unavailable")
    }
}

impl std::error::Error for ProviderUnavailable {}

pub struct TokenInput<'a> {
    pub encoded: &'a str,
    pub nonce: &'a Nonce,
    pub access_token: &'a AccessToken,
    pub now_ms: i64,
}

pub struct Verified {
    pub subject: String,
    pub claims: Value,
}

impl fmt::Debug for Verified {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Verified").finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "oidc_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "oidc_flow_tests.rs"]
mod flow_tests;

#[cfg(test)]
pub use flow_tests::FixtureProvider;

#[cfg(test)]
#[path = "oidc_candidate_cache_tests.rs"]
mod candidate_cache_tests;

#[cfg(test)]
#[path = "oidc_budget_tests.rs"]
mod budget_tests;
