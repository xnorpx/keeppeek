//! Stores revocable browser sessions without retaining bearer cookie values.

use anyhow::{Context, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fmt,
    net::IpAddr,
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use uuid::Uuid;

// Anonymous sessions share the global limit but have a tighter login-flood budget.
const SESSION_LIMIT: usize = 4_096;
const BOOTSTRAP_LIMIT: usize = 256;
const BOOTSTRAP_LIFETIME: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub identity_id: Uuid,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub idle: Duration,
    pub absolute: Duration,
    pub per_identity: usize,
    pub per_address: usize,
}

#[derive(Clone)]
pub struct Session {
    pub id: Uuid,
    pub binding: Option<Binding>,
    pub origin: String,
    pub address: IpAddr,
    pub created_at_ms: i64,
    pub absolute_expires_at_ms: i64,
    handle_hash: [u8; 32],
    csrf: String,
    created_at: Instant,
    last_activity: Instant,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserSession")
            .field("id", &self.id)
            .field("binding", &self.binding)
            .finish_non_exhaustive()
    }
}

impl Session {
    pub(crate) fn last_activity_at_ms(&self) -> i64 {
        self.created_at_ms.saturating_add(
            i64::try_from(
                self.last_activity
                    .saturating_duration_since(self.created_at)
                    .as_millis(),
            )
            .unwrap_or(i64::MAX),
        )
    }

    pub(crate) fn csrf(&self) -> &str {
        &self.csrf
    }

    pub(crate) fn csrf_matches(&self, provided: &str) -> bool {
        let actual: [u8; 32] = Sha256::digest(provided.as_bytes()).into();
        let expected: [u8; 32] = Sha256::digest(self.csrf.as_bytes()).into();
        bool::from(actual.ct_eq(&expected))
    }

    fn expired(&self, now: Instant, limits: Limits) -> bool {
        let (idle, absolute) = if self.binding.is_some() {
            (limits.idle, limits.absolute)
        } else {
            (BOOTSTRAP_LIFETIME, BOOTSTRAP_LIFETIME)
        };
        now.saturating_duration_since(self.created_at) >= absolute
            || now.saturating_duration_since(self.last_activity) >= idle
    }
}

pub struct Issued {
    pub cookie: String,
    pub csrf: String,
    pub session_id: Uuid,
}

impl fmt::Debug for Issued {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IssuedBrowserSession")
            .field("id", &self.session_id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct Sessions {
    records: HashMap<Uuid, Session>,
    limits: Limits,
}

impl Sessions {
    pub(crate) fn new(limits: Limits) -> Self {
        assert!(
            !limits.idle.is_zero() && limits.idle <= limits.absolute,
            "invalid browser session timeouts"
        );
        assert!(
            limits.per_identity > 0 && limits.per_address > 0,
            "invalid browser session capacity"
        );
        Self {
            records: HashMap::new(),
            limits,
        }
    }

    pub(crate) fn issue(
        &mut self,
        binding: Option<Binding>,
        origin: &str,
        address: IpAddr,
        now: Instant,
        now_ms: i64,
    ) -> anyhow::Result<Issued> {
        self.expire(now);
        let address = super::normalize_address(address);
        self.require_capacity(binding, address)?;
        let lifetime = if binding.is_some() {
            self.limits.absolute
        } else {
            BOOTSTRAP_LIFETIME
        };
        let expires_at_ms = now_ms
            .checked_add(i64::try_from(lifetime.as_millis())?)
            .context("browser session expiry overflow")?;
        ensure!(now_ms >= 0, "invalid browser session clock");
        let id = Uuid::new_v4();
        let cookie = format!("{}.{}", id.simple(), random_token());
        let csrf = random_token();
        let session = Session {
            id,
            binding,
            origin: origin.to_owned(),
            address,
            created_at_ms: now_ms,
            absolute_expires_at_ms: expires_at_ms,
            handle_hash: Sha256::digest(cookie.as_bytes()).into(),
            csrf: csrf.clone(),
            created_at: now,
            last_activity: now,
        };
        assert!(
            !self.records.contains_key(&id),
            "browser session ID collision"
        );
        self.records.insert(id, session);
        Ok(Issued {
            cookie,
            csrf,
            session_id: id,
        })
    }

    fn require_capacity(&self, binding: Option<Binding>, address: IpAddr) -> anyhow::Result<()> {
        ensure!(
            self.records.len() < SESSION_LIMIT,
            "browser session capacity reached"
        );
        ensure!(
            self.records
                .values()
                .filter(|s| s.address == address)
                .count()
                < self.limits.per_address,
            "browser session address limit reached"
        );
        match binding {
            Some(binding) => {
                ensure!(
                    self.records
                        .values()
                        .filter(|s| s
                            .binding
                            .is_some_and(|b| b.identity_id == binding.identity_id))
                        .count()
                        < self.limits.per_identity,
                    "browser session identity limit reached"
                );
            }
            None => ensure!(
                self.records
                    .values()
                    .filter(|s| s.binding.is_none())
                    .count()
                    < BOOTSTRAP_LIMIT,
                "browser bootstrap capacity reached"
            ),
        }
        Ok(())
    }

    pub(crate) fn rotate(
        &mut self,
        prior: Uuid,
        binding: Binding,
        now: Instant,
        now_ms: i64,
    ) -> anyhow::Result<Issued> {
        let previous = self
            .records
            .remove(&prior)
            .context("browser session is unavailable")?;
        ensure!(
            !previous.expired(now, self.limits),
            "browser session expired"
        );
        match self.issue(
            Some(binding),
            &previous.origin,
            previous.address,
            now,
            now_ms,
        ) {
            Ok(issued) => Ok(issued),
            Err(error) => {
                self.records.insert(prior, previous);
                Err(error)
            }
        }
    }

    pub(crate) fn authenticate(
        &mut self,
        cookie: &str,
        origin: &str,
        now: Instant,
    ) -> Option<Session> {
        let authenticated = self.lookup(cookie, origin, now)?;
        let session = self
            .records
            .get_mut(&authenticated.id)
            .expect("authenticated session remains present");
        session.last_activity = session.last_activity.max(now);
        Some(session.clone())
    }

    pub(crate) fn lookup(&mut self, cookie: &str, origin: &str, now: Instant) -> Option<Session> {
        let id = cookie_id(cookie)?;
        let session = self.records.get(&id)?;
        if session.expired(now, self.limits) {
            self.records.remove(&id);
            return None;
        }
        let hash: [u8; 32] = Sha256::digest(cookie.as_bytes()).into();
        if session.origin != origin || !bool::from(session.handle_hash.ct_eq(&hash)) {
            return None;
        }
        Some(session.clone())
    }

    pub(crate) fn active(&self, id: Uuid, binding: Binding, now: Instant) -> bool {
        self.records
            .get(&id)
            .is_some_and(|s| s.binding == Some(binding) && !s.expired(now, self.limits))
    }

    pub(crate) fn touch(&mut self, id: Uuid, binding: Binding, now: Instant) -> bool {
        if !self.active(id, binding, now) {
            return false;
        }
        self.records
            .get_mut(&id)
            .expect("active browser session remains present")
            .last_activity = now;
        true
    }

    pub(crate) fn revoke(&mut self, id: Uuid) {
        self.records.remove(&id);
    }

    pub(crate) fn revoke_identity(&mut self, identity: Uuid) {
        self.records
            .retain(|_, session| !session.binding.is_some_and(|b| b.identity_id == identity));
    }

    pub(crate) fn expire(&mut self, now: Instant) {
        self.records
            .retain(|_, session| !session.expired(now, self.limits));
    }

    pub(crate) fn list(&self) -> impl Iterator<Item = &Session> {
        self.records.values()
    }
}

fn cookie_id(cookie: &str) -> Option<Uuid> {
    // UUID (32), separator (1), and unpadded 256-bit base64url secret (43).
    if cookie.len() != 76 || !cookie.is_ascii() {
        return None;
    }
    let (id, secret) = cookie.split_once('.')?;
    if id.len() != 32 || secret.len() != 43 {
        return None;
    }
    Uuid::parse_str(id).ok()
}

pub fn random_token() -> String {
    URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
}

#[cfg(test)]
#[path = "browser_sessions_tests.rs"]
mod tests;
