//! Binds single-use OIDC login state to the initiating browser and configuration.

use super::{external::Oidc, oidc_transport::Policy};
use anyhow::{Result, anyhow, ensure};
use oauth2::{
    AuthUrl, ClientId, CsrfToken, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope,
    basic::BasicClient,
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fmt,
    time::{Duration, Instant},
};
use url::Url;
use uuid::Uuid;

const LIMIT: usize = 256;
const LIFETIME: Duration = Duration::from_secs(300);

pub struct Start<'a> {
    pub browser: Uuid,
    pub origin: &'a str,
    pub provider_id: &'a str,
    pub revision: u64,
    pub return_path: &'a str,
    pub candidate_plan: Option<Uuid>,
}

pub struct Transaction {
    pub browser: Uuid,
    pub origin: String,
    pub provider_id: String,
    pub revision: u64,
    pub return_path: String,
    pub candidate_plan: Option<Uuid>,
    pub nonce: CsrfToken,
    pub verifier: PkceCodeVerifier,
    created: Instant,
}

impl fmt::Debug for Transaction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoginTransaction")
            .finish_non_exhaustive()
    }
}

#[derive(Default, Debug)]
pub struct Transactions {
    pending: HashMap<[u8; 32], Transaction>,
}

impl Transactions {
    pub(crate) fn candidate(&self, state: &str) -> Option<Uuid> {
        if state.is_empty() || state.len() > 128 {
            return None;
        }
        let digest: [u8; 32] = Sha256::digest(state.as_bytes()).into();
        self.pending.get(&digest)?.candidate_plan
    }

    pub(crate) fn start(
        &mut self,
        input: Start<'_>,
        config: &Oidc,
        authorization_url: &str,
        now: Instant,
    ) -> Result<Url> {
        self.require_capacity(input.browser, now)?;
        validate_return_path(input.return_path)?;
        ensure!(
            input.provider_id.len() <= 64 && !input.provider_id.is_empty() && input.revision > 0,
            "invalid login binding"
        );
        let redirect = Url::parse(&config.redirect_uri)?;
        ensure!(
            redirect.origin().ascii_serialization() == input.origin,
            "login origin mismatch"
        );
        Policy::new(config)?.endpoint(authorization_url)?;
        let client = BasicClient::new(ClientId::new(config.client_id.clone()))
            .set_auth_uri(AuthUrl::new(authorization_url.to_owned())?)
            .set_redirect_uri(RedirectUrl::new(config.redirect_uri.clone())?);
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let nonce = CsrfToken::new_random();
        let (url, state) = client
            .authorize_url(CsrfToken::new_random)
            .add_extra_param("nonce", nonce.secret())
            .add_scope(Scope::new("openid".into()))
            .add_scopes(
                config
                    .scopes
                    .iter()
                    .filter(|scope| scope.as_str() != "openid")
                    .cloned()
                    .map(Scope::new),
            )
            .set_pkce_challenge(challenge)
            .url();
        let digest: [u8; 32] = Sha256::digest(state.secret().as_bytes()).into();
        let transaction = Transaction {
            browser: input.browser,
            origin: input.origin.to_owned(),
            provider_id: input.provider_id.to_owned(),
            revision: input.revision,
            return_path: input.return_path.to_owned(),
            candidate_plan: input.candidate_plan,
            nonce,
            verifier,
            created: now,
        };
        assert!(
            self.pending.insert(digest, transaction).is_none(),
            "random login state collision"
        );
        Ok(url)
    }

    fn require_capacity(&mut self, browser: Uuid, now: Instant) -> Result<()> {
        self.pending
            .retain(|_, transaction| now.saturating_duration_since(transaction.created) < LIFETIME);
        ensure!(
            self.pending.len() < LIMIT,
            "pending login capacity exceeded"
        );
        ensure!(
            self.pending
                .values()
                .filter(|transaction| transaction.browser == browser)
                .count()
                < 2,
            "browser login capacity exceeded"
        );
        Ok(())
    }

    pub(crate) fn consume(
        &mut self,
        state: &str,
        browser: Uuid,
        origin: &str,
        revision: u64,
        now: Instant,
    ) -> Result<Transaction> {
        ensure!(
            !state.is_empty() && state.len() <= 128,
            "invalid login state"
        );
        let digest: [u8; 32] = Sha256::digest(state.as_bytes()).into();
        // Consume before checking bindings so a failed callback cannot be retried.
        let transaction = self
            .pending
            .remove(&digest)
            .ok_or_else(|| anyhow!("login state is unavailable"))?;
        ensure!(
            transaction.browser == browser
                && transaction.origin == origin
                && transaction.revision == revision
                && now.saturating_duration_since(transaction.created) < LIFETIME,
            "login state binding is invalid or expired"
        );
        Ok(transaction)
    }

    pub(crate) fn revoke_browser(&mut self, browser: Uuid) {
        self.pending
            .retain(|_, transaction| transaction.browser != browser);
    }
}

pub fn validate_return_path(path: &str) -> Result<()> {
    // ponytail: UI routes need only plain relative paths, not nested encoded redirect targets.
    ensure!(
        path.len() <= 2_048
            && path.starts_with('/')
            && !path.starts_with("//")
            && !path.starts_with("/auth/")
            && path.is_ascii()
            && !path
                .bytes()
                .any(|byte| byte.is_ascii_control() || matches!(byte, b'\\' | b'%' | b'#' | b' ')),
        "invalid login return path"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(browser: uuid::Uuid) -> Start<'static> {
        Start {
            browser,
            origin: "https://keeppeek.example",
            provider_id: "company",
            revision: 1,
            return_path: "/events",
            candidate_plan: None,
        }
    }

    fn config() -> super::Oidc {
        toml::from_str(
            r#"
issuer = "https://identity.example"
client_id = "keeppeek"
redirect_uri = "https://keeppeek.example/auth/callback"
"#,
        )
        .unwrap()
    }

    #[test]
    fn authorization_request_binds_pkce_nonce_and_consumes_state_once() {
        let now = std::time::Instant::now();
        let browser = uuid::Uuid::new_v4();
        let mut transactions = Transactions::default();
        let url = transactions
            .start(
                input(browser),
                &config(),
                "https://identity.example/authorize",
                now,
            )
            .unwrap();
        let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["response_type"], "code");
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(query["scope"], "openid");
        let transaction = transactions
            .consume(&query["state"], browser, "https://keeppeek.example", 1, now)
            .unwrap();
        assert_eq!(transaction.nonce.secret(), &query["nonce"]);
        let challenge = PkceCodeChallenge::from_code_verifier_sha256(&transaction.verifier);
        assert_eq!(challenge.as_str(), query["code_challenge"]);
        assert!(
            transactions
                .consume(&query["state"], browser, "https://keeppeek.example", 1, now)
                .is_err()
        );
        assert!(!format!("{transaction:?}").contains(&query["nonce"]));
    }

    #[test]
    fn login_state_rejects_wrong_browser_origin_revision_and_expiry() {
        let now = std::time::Instant::now();
        let browser = uuid::Uuid::new_v4();
        for (claimed_browser, origin, revision, age) in [
            (uuid::Uuid::new_v4(), "https://keeppeek.example", 1, 0),
            (browser, "https://evil.example", 1, 0),
            (browser, "https://keeppeek.example", 2, 0),
            (browser, "https://keeppeek.example", 1, 300),
        ] {
            let mut transactions = Transactions::default();
            let url = transactions
                .start(
                    input(browser),
                    &config(),
                    "https://identity.example/authorize",
                    now,
                )
                .unwrap();
            let state = url
                .query_pairs()
                .find(|(name, _)| name == "state")
                .unwrap()
                .1
                .into_owned();
            assert!(
                transactions
                    .consume(
                        &state,
                        claimed_browser,
                        origin,
                        revision,
                        now + std::time::Duration::from_secs(age)
                    )
                    .is_err()
            );
            assert!(
                transactions
                    .consume(&state, browser, "https://keeppeek.example", 1, now)
                    .is_err()
            );
        }
    }

    #[test]
    fn return_paths_and_pending_login_work_are_bounded() {
        for path in [
            "https://evil.example",
            "//evil.example",
            "/\\evil",
            "/%2f%2fevil",
            "/\n",
            "/auth/callback",
            "relative",
        ] {
            assert!(validate_return_path(path).is_err(), "accepted {path}");
        }
        let now = std::time::Instant::now();
        let mut transactions = Transactions::default();
        for _ in 0..256 {
            transactions
                .start(
                    input(uuid::Uuid::new_v4()),
                    &config(),
                    "https://identity.example/authorize",
                    now,
                )
                .unwrap();
        }
        assert!(
            transactions
                .start(
                    input(uuid::Uuid::new_v4()),
                    &config(),
                    "https://identity.example/authorize",
                    now
                )
                .is_err()
        );
        assert!(
            transactions
                .start(
                    input(uuid::Uuid::new_v4()),
                    &config(),
                    "https://identity.example/authorize",
                    now + std::time::Duration::from_secs(300)
                )
                .is_ok()
        );
    }
}
