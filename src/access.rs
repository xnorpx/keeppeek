use anyhow::Context;
use ipnet::IpNet;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fmt,
    net::IpAddr,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;
use uuid::Uuid;

const LEGACY_ACCESS_CATALOG_NAME: &str = "access.toml";
const ACCESS_CATALOG_SECTION: &str = "access_credentials";
const ACCESS_CATALOG_VERSION: u32 = 1;
const MAX_CREDENTIALS: usize = 128;
const MAX_AUDIT_EVENTS: usize = 1_024;
const MAX_FAILED_AUTHENTICATION_ADDRESSES: usize = 1_024;
const MAX_FORWARDING_HOPS: usize = 16;
const MAX_CREDENTIAL_NAME_BYTES: usize = 64;
const MAX_CREDENTIAL_DESCRIPTION_BYTES: usize = 256;
const MAX_AUDIT_FIELD_BYTES: usize = 128;
const LAST_USED_WRITE_INTERVAL_MS: i64 = 60_000;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct AccessKey(u128);

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct AccessKeyFingerprint([u8; 32]);

impl AccessKeyFingerprint {
    pub fn matches(self, other: Self) -> bool {
        bool::from(self.0.ct_eq(&other.0))
    }
}

impl AccessKey {
    pub const fn unset() -> Self {
        Self(0)
    }

    pub const fn is_unset(self) -> bool {
        self.0 == 0
    }

    pub fn generate() -> Self {
        loop {
            let key = Self(Uuid::new_v4().as_u128());
            if !key.is_unset() {
                return key;
            }
        }
    }

    pub fn parse(value: &str) -> Result<Self, uuid::Error> {
        if value == "0" {
            return Ok(Self::unset());
        }
        Uuid::parse_str(value).map(|uuid| Self(uuid.as_u128()))
    }

    pub fn canonical(self) -> String {
        Uuid::from_u128(self.0).hyphenated().to_string()
    }

    pub fn fingerprint(self) -> AccessKeyFingerprint {
        AccessKeyFingerprint(Sha256::digest(self.0.to_be_bytes()).into())
    }
}

impl fmt::Debug for AccessKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccessKey([redacted])")
    }
}

impl Serialize for AccessKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if self.is_unset() {
            serializer.serialize_u64(0)
        } else {
            serializer.serialize_str(&self.canonical())
        }
    }
}

impl<'de> Deserialize<'de> for AccessKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct AccessKeyVisitor;

        impl de::Visitor<'_> for AccessKeyVisitor {
            type Value = AccessKey;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a hyphenated UUID string or the integer 0")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                AccessKey::parse(value).map_err(E::custom)
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value == 0 {
                    Ok(AccessKey::unset())
                } else {
                    Err(E::custom("integer access keys must be 0"))
                }
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value == 0 {
                    Ok(AccessKey::unset())
                } else {
                    Err(E::custom("integer access keys must be 0"))
                }
            }
        }

        deserializer.deserialize_any(AccessKeyVisitor)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessRole {
    Administrator,
    User,
}

impl AccessRole {
    pub(crate) const fn permits(self, required: Self) -> bool {
        matches!(self, Self::Administrator) || matches!(required, Self::User)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientClassificationReason {
    DirectLocal,
    DirectRemote,
    TrustedProxyLocal,
    TrustedProxyRemote,
    UntrustedForwarding,
    MissingForwardedClient,
    MalformedForwardedClient,
    UnknownSession,
}

impl ClientClassificationReason {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::DirectLocal => "direct_local",
            Self::DirectRemote => "direct_remote",
            Self::TrustedProxyLocal => "trusted_proxy_local",
            Self::TrustedProxyRemote => "trusted_proxy_remote",
            Self::UntrustedForwarding => "untrusted_forwarding",
            Self::MissingForwardedClient => "missing_forwarded_client",
            Self::MalformedForwardedClient => "malformed_forwarded_client",
            Self::UnknownSession => "unknown_session",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientClassification {
    pub(crate) peer_address: IpAddr,
    pub(crate) effective_address: IpAddr,
    pub(crate) local: bool,
    pub(crate) reason: ClientClassificationReason,
}

#[derive(Clone)]
pub struct NetworkAccessPolicy {
    local_networks: Arc<[IpNet]>,
    trusted_proxies: Arc<[IpNet]>,
}

impl NetworkAccessPolicy {
    pub(crate) fn new(local_networks: Vec<IpNet>, trusted_proxies: Vec<IpNet>) -> Self {
        Self {
            local_networks: local_networks.into(),
            trusted_proxies: trusted_proxies.into(),
        }
    }

    pub(crate) fn classify<'a>(
        &self,
        peer_address: IpAddr,
        headers: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> ClientClassification {
        let peer_address = normalize_address(peer_address);
        let mut forwarded_client = None;
        let mut forwarded_client_count = 0;
        let mut has_other_forwarding_header = false;
        for (name, value) in headers {
            if name.eq_ignore_ascii_case("X-Forwarded-For") {
                forwarded_client_count += 1;
                forwarded_client = Some(value);
            } else if is_forwarding_header(name) {
                has_other_forwarding_header = true;
            }
        }
        let has_forwarding_header = forwarded_client_count != 0 || has_other_forwarding_header;
        let trusted_peer = self.contains(&self.trusted_proxies, peer_address);

        if has_forwarding_header && !trusted_peer {
            return ClientClassification {
                peer_address,
                effective_address: peer_address,
                local: false,
                reason: ClientClassificationReason::UntrustedForwarding,
            };
        }
        if trusted_peer {
            if forwarded_client_count == 0 {
                return ClientClassification {
                    peer_address,
                    effective_address: peer_address,
                    local: false,
                    reason: ClientClassificationReason::MissingForwardedClient,
                };
            }
            if forwarded_client_count != 1 || has_other_forwarding_header {
                return ClientClassification {
                    peer_address,
                    effective_address: peer_address,
                    local: false,
                    reason: ClientClassificationReason::MalformedForwardedClient,
                };
            }
            let Some(chain) = forwarded_client.and_then(parse_forwarded_chain) else {
                return ClientClassification {
                    peer_address,
                    effective_address: peer_address,
                    local: false,
                    reason: ClientClassificationReason::MalformedForwardedClient,
                };
            };
            let mut effective_address = peer_address;
            for address in chain.into_iter().rev() {
                if !self.contains(&self.trusted_proxies, effective_address) {
                    break;
                }
                effective_address = address;
            }
            let local = self.contains(&self.local_networks, effective_address);
            return ClientClassification {
                peer_address,
                effective_address,
                local,
                reason: if local {
                    ClientClassificationReason::TrustedProxyLocal
                } else {
                    ClientClassificationReason::TrustedProxyRemote
                },
            };
        }

        let local = self.contains(&self.local_networks, peer_address);
        ClientClassification {
            peer_address,
            effective_address: peer_address,
            local,
            reason: if local {
                ClientClassificationReason::DirectLocal
            } else {
                ClientClassificationReason::DirectRemote
            },
        }
    }

    fn contains(&self, networks: &[IpNet], address: IpAddr) -> bool {
        networks.iter().any(|network| network.contains(&address))
    }
}

impl Default for NetworkAccessPolicy {
    fn default() -> Self {
        Self::new(default_local_networks(), Vec::new())
    }
}

pub fn default_local_networks() -> Vec<IpNet> {
    [
        "127.0.0.0/8",
        "10.0.0.0/8",
        "172.16.0.0/12",
        "192.168.0.0/16",
        "169.254.0.0/16",
        "::1/128",
        "fc00::/7",
        "fe80::/10",
    ]
    .into_iter()
    .map(|network| IpNet::from_str(network).expect("default local network must be valid"))
    .collect()
}

fn normalize_address(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(address) => address
            .to_ipv4_mapped()
            .map_or(IpAddr::V6(address), IpAddr::V4),
        address => address,
    }
}

fn is_forwarding_header(name: &str) -> bool {
    [
        "Forwarded",
        "X-Forwarded-Host",
        "X-Forwarded-Proto",
        "X-Real-IP",
    ]
    .iter()
    .any(|forwarding| name.eq_ignore_ascii_case(forwarding))
}

fn parse_forwarded_chain(value: &str) -> Option<Vec<IpAddr>> {
    let values = value.split(',').collect::<Vec<_>>();
    if values.is_empty() || values.len() > MAX_FORWARDING_HOPS {
        return None;
    }
    values
        .into_iter()
        .map(|value| value.trim().parse().ok().map(normalize_address))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialMetadata {
    pub(crate) id: Uuid,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) role: AccessRole,
    pub(crate) created_at_ms: i64,
    pub(crate) rotated_at_ms: Option<i64>,
    pub(crate) last_used_at_ms: Option<i64>,
    pub(crate) expires_at_ms: Option<i64>,
    pub(crate) disabled: bool,
    pub(crate) revoked_at_ms: Option<i64>,
    pub(crate) revision: u64,
    pub(crate) initial_access_key_pending: bool,
}

#[derive(Clone, Debug)]
pub struct IssuedCredential {
    pub(crate) metadata: CredentialMetadata,
    pub(crate) access_key: AccessKey,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticatedCredential {
    pub(crate) id: Uuid,
    pub(crate) name: String,
    pub(crate) role: AccessRole,
    pub(crate) revision: u64,
    pub(crate) expires_at_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthenticationFailure {
    Missing,
    Malformed,
    Invalid,
    Disabled,
    Revoked,
    Expired,
    RateLimited,
}

impl AuthenticationFailure {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Malformed => "malformed",
            Self::Invalid => "invalid",
            Self::Disabled => "disabled",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
            Self::RateLimited => "rate_limited",
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct AccessAuditEvent {
    pub(crate) id: Uuid,
    pub(crate) timestamp_ms: i64,
    pub(crate) principal_id: Option<String>,
    pub(crate) role: Option<AccessRole>,
    pub(crate) action: String,
    pub(crate) target_id: Option<String>,
    pub(crate) result: String,
    pub(crate) client_classification: String,
}

pub struct NewAccessAuditEvent<'a> {
    pub(crate) timestamp_ms: i64,
    pub(crate) principal_id: Option<&'a str>,
    pub(crate) role: Option<AccessRole>,
    pub(crate) action: &'a str,
    pub(crate) target_id: Option<&'a str>,
    pub(crate) result: &'a str,
    pub(crate) client_classification: ClientClassificationReason,
}

#[derive(Clone, Deserialize, Serialize)]
struct StoredCredential {
    id: Uuid,
    name: String,
    description: Option<String>,
    role: AccessRole,
    verifier: AccessKeyFingerprint,
    created_at_ms: i64,
    #[serde(default)]
    rotated_at_ms: Option<i64>,
    last_used_at_ms: Option<i64>,
    expires_at_ms: Option<i64>,
    disabled: bool,
    revoked_at_ms: Option<i64>,
    revision: u64,
    legacy: bool,
    initial_secret_pending: bool,
}

impl StoredCredential {
    fn metadata(&self) -> CredentialMetadata {
        CredentialMetadata {
            id: self.id,
            name: self.name.clone(),
            description: self.description.clone(),
            role: self.role,
            created_at_ms: self.created_at_ms,
            rotated_at_ms: self.rotated_at_ms,
            last_used_at_ms: self.last_used_at_ms,
            expires_at_ms: self.expires_at_ms,
            disabled: self.disabled,
            revoked_at_ms: self.revoked_at_ms,
            revision: self.revision,
            initial_access_key_pending: self.initial_secret_pending,
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
struct AccessCatalog {
    version: u32,
    credentials: Vec<StoredCredential>,
    audit: Vec<AccessAuditEvent>,
}

#[derive(Deserialize, Serialize)]
struct PersistedAccessCatalog {
    version: u32,
    credentials: Vec<StoredCredential>,
}

impl Default for AccessCatalog {
    fn default() -> Self {
        Self {
            version: ACCESS_CATALOG_VERSION,
            credentials: Vec::new(),
            audit: Vec::new(),
        }
    }
}

pub fn validate_backup_catalog_document(bytes: &[u8]) -> anyhow::Result<()> {
    let catalog = toml::from_str::<AccessCatalog>(std::str::from_utf8(bytes)?)?;
    validate_catalog(&catalog)?;
    if !catalog.audit.is_empty()
        || catalog
            .credentials
            .iter()
            .any(|credential| credential.last_used_at_ms.is_some())
    {
        anyhow::bail!("backup access catalog contains non-recovery activity");
    }
    Ok(())
}

struct FailedAuthenticationWindow {
    started_at: Instant,
    attempts: u32,
}

struct AccessState {
    config_path: Option<PathBuf>,
    config_update: Arc<Mutex<()>>,
    catalog: AccessCatalog,
    failed_authentication: HashMap<IpAddr, FailedAuthenticationWindow>,
    failed_authentication_limit: u32,
    failed_authentication_window: Duration,
}

#[derive(Clone)]
pub struct AccessManager {
    state: Arc<Mutex<AccessState>>,
}

impl AccessManager {
    pub(crate) fn ephemeral(access_key: AccessKey) -> Self {
        let mut catalog = AccessCatalog::default();
        if !access_key.is_unset() {
            catalog
                .credentials
                .push(legacy_credential(access_key, now_ms()));
        }
        Self::from_catalog(None, catalog, Arc::new(Mutex::new(())))
    }

    #[cfg(test)]
    pub(crate) fn open(config_path: &Path, access_key: AccessKey) -> anyhow::Result<Self> {
        Self::open_with_config_update(config_path, access_key, Arc::new(Mutex::new(())))
    }

    pub(crate) fn open_with_config_update(
        config_path: &Path,
        access_key: AccessKey,
        config_update: Arc<Mutex<()>>,
    ) -> anyhow::Result<Self> {
        let legacy_path = config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(LEGACY_ACCESS_CATALOG_NAME);
        let configured = load_configured_catalog(config_path)?;
        let migrate_legacy = configured.is_none() && legacy_path.is_file();
        let mut catalog = if let Some(catalog) = configured {
            catalog
        } else if migrate_legacy {
            load_legacy_catalog(&legacy_path)?
        } else {
            AccessCatalog::default()
        };
        validate_catalog(&catalog)?;
        catalog.audit.clear();
        for credential in &mut catalog.credentials {
            credential.last_used_at_ms = None;
        }
        let mut changed = migrate_legacy || catalog.credentials.is_empty();
        if !access_key.is_unset() {
            if let Some(credential) = catalog
                .credentials
                .iter_mut()
                .find(|credential| credential.legacy)
            {
                if credential.revoked_at_ms.is_none()
                    && !credential.verifier.matches(access_key.fingerprint())
                {
                    credential.verifier = access_key.fingerprint();
                    credential.revision = credential.revision.saturating_add(1);
                    changed = true;
                }
            } else {
                catalog
                    .credentials
                    .push(legacy_credential(access_key, now_ms()));
                changed = true;
            }
        }
        let manager = Self::from_catalog(Some(config_path.to_owned()), catalog, config_update);
        if changed {
            manager.persist()?;
        }
        if legacy_path.exists() {
            std::fs::remove_file(&legacy_path)
                .with_context(|| format!("unable to remove {}", legacy_path.display()))?;
        }
        Ok(manager)
    }

    pub(crate) fn configure_rate_limit(&self, attempts: u32, window: Duration) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.failed_authentication_limit = attempts.max(1);
        state.failed_authentication_window = window.max(Duration::from_secs(1));
    }

    pub(crate) fn authenticate(
        &self,
        address: IpAddr,
        authorization: &[&str],
        now: i64,
        now_instant: Instant,
    ) -> Result<AuthenticatedCredential, AuthenticationFailure> {
        let address = normalize_address(address);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if authentication_is_rate_limited(&mut state, address, now_instant) {
            return Err(AuthenticationFailure::RateLimited);
        }
        let failure = if authorization.is_empty() {
            Some(AuthenticationFailure::Missing)
        } else if authorization.len() != 1 {
            Some(AuthenticationFailure::Malformed)
        } else {
            None
        };
        let candidate = failure
            .is_none()
            .then(|| {
                let mut parts = authorization[0].split_ascii_whitespace();
                match (parts.next(), parts.next(), parts.next()) {
                    (Some(scheme), Some(value), None) if scheme.eq_ignore_ascii_case("Bearer") => {
                        AccessKey::parse(value).ok().filter(|key| !key.is_unset())
                    }
                    _ => None,
                }
            })
            .flatten();
        let mut matched_index = None;
        if let Some(candidate) = candidate {
            let fingerprint = candidate.fingerprint();
            for (index, credential) in state.catalog.credentials.iter().enumerate() {
                if credential.verifier.matches(fingerprint) {
                    matched_index = Some(index);
                }
            }
        }
        let failure = failure.or_else(|| {
            let credential = matched_index.and_then(|index| state.catalog.credentials.get(index));
            match credential {
                None => Some(AuthenticationFailure::Invalid),
                Some(credential) if credential.revoked_at_ms.is_some() => {
                    Some(AuthenticationFailure::Revoked)
                }
                Some(credential) if credential.disabled => Some(AuthenticationFailure::Disabled),
                Some(credential)
                    if credential
                        .expires_at_ms
                        .is_some_and(|expires_at| expires_at <= now) =>
                {
                    Some(AuthenticationFailure::Expired)
                }
                Some(_) => None,
            }
        });
        if let Some(failure) = failure {
            register_authentication_failure(&mut state, address, now_instant);
            return Err(failure);
        }
        state.failed_authentication.remove(&address);
        let index = matched_index.expect("successful authentication must match a credential");
        let credential = &mut state.catalog.credentials[index];
        let authenticated = AuthenticatedCredential {
            id: credential.id,
            name: credential.name.clone(),
            role: credential.role,
            revision: credential.revision,
            expires_at_ms: credential.expires_at_ms,
        };
        if credential
            .last_used_at_ms
            .is_none_or(|last_used| now.saturating_sub(last_used) >= LAST_USED_WRITE_INTERVAL_MS)
        {
            credential.last_used_at_ms = Some(now);
        }
        Ok(authenticated)
    }

    pub(crate) fn credential_is_active(&self, id: Uuid, revision: u64, now: i64) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.catalog.credentials.iter().any(|credential| {
            credential.id == id
                && credential.revision == revision
                && !credential.disabled
                && credential.revoked_at_ms.is_none()
                && credential
                    .expires_at_ms
                    .is_none_or(|expires_at| expires_at > now)
        })
    }

    pub(crate) fn list_credentials(&self) -> Vec<CredentialMetadata> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .catalog
            .credentials
            .iter()
            .map(StoredCredential::metadata)
            .collect()
    }

    pub(crate) fn create_credential(
        &self,
        name: &str,
        description: Option<&str>,
        role: AccessRole,
        expires_at_ms: Option<i64>,
        now: i64,
    ) -> anyhow::Result<IssuedCredential> {
        let name = validated_name(name)?;
        let description = validated_description(description)?;
        if expires_at_ms.is_some_and(|expires_at| expires_at <= now) {
            anyhow::bail!("credential expiry must be in the future");
        }
        let access_key = AccessKey::generate();
        let credential = StoredCredential {
            id: Uuid::new_v4(),
            name,
            description,
            role,
            verifier: access_key.fingerprint(),
            created_at_ms: now,
            rotated_at_ms: None,
            last_used_at_ms: None,
            expires_at_ms,
            disabled: false,
            revoked_at_ms: None,
            revision: 1,
            legacy: false,
            initial_secret_pending: false,
        };
        let metadata = credential.metadata();
        self.mutate_catalog(|catalog| {
            if catalog.credentials.len() >= MAX_CREDENTIALS {
                anyhow::bail!("credential limit reached");
            }
            if catalog.credentials.iter().any(|existing| {
                existing.revoked_at_ms.is_none()
                    && existing.name.eq_ignore_ascii_case(&credential.name)
            }) {
                anyhow::bail!("an active credential already uses that name");
            }
            catalog.credentials.push(credential);
            Ok(())
        })?;
        Ok(IssuedCredential {
            metadata,
            access_key,
        })
    }

    pub(crate) fn rotate_credential(&self, id: Uuid, now: i64) -> anyhow::Result<IssuedCredential> {
        let access_key = AccessKey::generate();
        self.replace_credential_key(id, access_key, now)
    }

    pub(crate) fn replace_credential_key(
        &self,
        id: Uuid,
        access_key: AccessKey,
        now: i64,
    ) -> anyhow::Result<IssuedCredential> {
        let metadata = self.mutate_catalog(|catalog| {
            let credential = credential_mut(catalog, id)?;
            if credential.revoked_at_ms.is_some() {
                anyhow::bail!("revoked credentials cannot be rotated");
            }
            credential.verifier = access_key.fingerprint();
            credential.revision = credential.revision.saturating_add(1);
            credential.rotated_at_ms = Some(now);
            credential.last_used_at_ms = None;
            credential.initial_secret_pending = false;
            Ok(credential.metadata())
        })?;
        Ok(IssuedCredential {
            metadata,
            access_key,
        })
    }

    pub(crate) fn set_credential_enabled(
        &self,
        id: Uuid,
        enabled: bool,
    ) -> anyhow::Result<CredentialMetadata> {
        self.mutate_catalog(|catalog| {
            let credential = credential_mut(catalog, id)?;
            if credential.revoked_at_ms.is_some() {
                anyhow::bail!("revoked credentials cannot be changed");
            }
            credential.disabled = !enabled;
            credential.revision = credential.revision.saturating_add(1);
            Ok(credential.metadata())
        })
    }

    pub(crate) fn revoke_credential(
        &self,
        id: Uuid,
        now: i64,
    ) -> anyhow::Result<CredentialMetadata> {
        self.mutate_catalog(|catalog| {
            let credential = credential_mut(catalog, id)?;
            if credential.revoked_at_ms.is_none() {
                credential.revoked_at_ms = Some(now);
                credential.revision = credential.revision.saturating_add(1);
            }
            Ok(credential.metadata())
        })
    }

    pub(crate) fn is_legacy_credential(&self, id: Uuid) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .catalog
            .credentials
            .iter()
            .any(|credential| credential.id == id && credential.legacy)
    }

    pub(crate) fn legacy_credential_id(&self) -> Option<Uuid> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .catalog
            .credentials
            .iter()
            .find(|credential| credential.legacy)
            .map(|credential| credential.id)
    }

    pub(crate) fn claim_initial_access_key(
        &self,
        access_key: AccessKey,
    ) -> anyhow::Result<IssuedCredential> {
        let metadata = self.mutate_catalog(|catalog| {
            let credential = catalog
                .credentials
                .iter_mut()
                .find(|credential| credential.legacy)
                .ok_or_else(|| {
                    anyhow::anyhow!("initial Administrator credential is unavailable")
                })?;
            if !credential.initial_secret_pending {
                anyhow::bail!("initial Administrator credential was already retrieved");
            }
            if !credential.verifier.matches(access_key.fingerprint()) {
                anyhow::bail!(
                    "initial Administrator credential does not match the protected secret"
                );
            }
            credential.initial_secret_pending = false;
            Ok(credential.metadata())
        })?;
        Ok(IssuedCredential {
            metadata,
            access_key,
        })
    }

    pub(crate) fn record_audit(&self, record: NewAccessAuditEvent<'_>) {
        let event = AccessAuditEvent {
            id: Uuid::new_v4(),
            timestamp_ms: record.timestamp_ms,
            principal_id: record
                .principal_id
                .map(|value| bounded_field(value, MAX_AUDIT_FIELD_BYTES)),
            role: record.role,
            action: bounded_field(record.action, MAX_AUDIT_FIELD_BYTES),
            target_id: record
                .target_id
                .map(|value| bounded_field(value, MAX_AUDIT_FIELD_BYTES)),
            result: bounded_field(record.result, MAX_AUDIT_FIELD_BYTES),
            client_classification: record.client_classification.as_str().to_owned(),
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.catalog.audit.len() == MAX_AUDIT_EVENTS {
            state.catalog.audit.remove(0);
        }
        state.catalog.audit.push(event);
        tracing::info!(
            event = "access_audit",
            action = record.action,
            result = record.result,
            principal_id = record.principal_id.unwrap_or(""),
            target_id = record.target_id.unwrap_or(""),
            client_classification = record.client_classification.as_str(),
        );
    }

    pub(crate) fn list_audit(&self, limit: usize) -> Vec<AccessAuditEvent> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let start = state
            .catalog
            .audit
            .len()
            .saturating_sub(limit.clamp(1, 200));
        state.catalog.audit[start..].to_vec()
    }

    pub(crate) const fn flush_audit(&self, _force: bool) -> anyhow::Result<()> {
        Ok(())
    }

    fn from_catalog(
        config_path: Option<PathBuf>,
        catalog: AccessCatalog,
        config_update: Arc<Mutex<()>>,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(AccessState {
                config_path,
                config_update,
                catalog,
                failed_authentication: HashMap::new(),
                failed_authentication_limit: 5,
                failed_authentication_window: Duration::from_secs(60),
            })),
        }
    }

    fn mutate_catalog<T>(
        &self,
        mutate: impl FnOnce(&mut AccessCatalog) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let config_update = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .config_update
            .clone();
        let _config_update = config_update
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = state.catalog.clone();
        let result = match mutate(&mut state.catalog) {
            Ok(result) => result,
            Err(error) => {
                state.catalog = previous;
                return Err(error);
            }
        };
        if let Err(error) = persist_catalog(&state) {
            state.catalog = previous;
            return Err(error);
        }
        Ok(result)
    }

    fn persist(&self) -> anyhow::Result<()> {
        let config_update = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .config_update
            .clone();
        let _config_update = config_update
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        persist_catalog(&state)
    }
}

fn legacy_credential(access_key: AccessKey, created_at_ms: i64) -> StoredCredential {
    StoredCredential {
        id: Uuid::new_v4(),
        name: "Initial Administrator".to_owned(),
        description: Some("First-run remote Administrator credential".to_owned()),
        role: AccessRole::Administrator,
        verifier: access_key.fingerprint(),
        created_at_ms,
        rotated_at_ms: None,
        last_used_at_ms: None,
        expires_at_ms: None,
        disabled: false,
        revoked_at_ms: None,
        revision: 1,
        legacy: true,
        initial_secret_pending: true,
    }
}

fn credential_mut(catalog: &mut AccessCatalog, id: Uuid) -> anyhow::Result<&mut StoredCredential> {
    catalog
        .credentials
        .iter_mut()
        .find(|credential| credential.id == id)
        .ok_or_else(|| anyhow::anyhow!("credential was not found"))
}

fn validated_name(name: &str) -> anyhow::Result<String> {
    let name = name.trim();
    if name.is_empty()
        || name.len() > MAX_CREDENTIAL_NAME_BYTES
        || name.chars().any(char::is_control)
    {
        anyhow::bail!("credential name must be 1 to {MAX_CREDENTIAL_NAME_BYTES} printable bytes");
    }
    Ok(name.to_owned())
}

fn validated_description(description: Option<&str>) -> anyhow::Result<Option<String>> {
    description
        .map(str::trim)
        .filter(|description| !description.is_empty())
        .map(|description| {
            if description.len() > MAX_CREDENTIAL_DESCRIPTION_BYTES
                || description.chars().any(char::is_control)
            {
                anyhow::bail!(
                    "credential description must be at most {MAX_CREDENTIAL_DESCRIPTION_BYTES} printable bytes"
                );
            }
            Ok(description.to_owned())
        })
        .transpose()
}

fn validate_catalog(catalog: &AccessCatalog) -> anyhow::Result<()> {
    if catalog.version != ACCESS_CATALOG_VERSION {
        anyhow::bail!("unsupported access catalog version {}", catalog.version);
    }
    if catalog.credentials.len() > MAX_CREDENTIALS || catalog.audit.len() > MAX_AUDIT_EVENTS {
        anyhow::bail!("access catalog exceeds its bounded record limits");
    }
    let mut ids = std::collections::HashSet::with_capacity(catalog.credentials.len());
    for credential in &catalog.credentials {
        validated_name(&credential.name)?;
        validated_description(credential.description.as_deref())?;
        if !ids.insert(credential.id) {
            anyhow::bail!("access catalog contains duplicate credential IDs");
        }
        if credential.revision == 0 {
            anyhow::bail!("access credential revision must be nonzero");
        }
    }
    Ok(())
}

fn authentication_is_rate_limited(state: &mut AccessState, address: IpAddr, now: Instant) -> bool {
    state.failed_authentication.retain(|_, window| {
        now.saturating_duration_since(window.started_at) < state.failed_authentication_window
    });
    state
        .failed_authentication
        .get(&address)
        .is_some_and(|window| window.attempts >= state.failed_authentication_limit)
}

fn register_authentication_failure(state: &mut AccessState, address: IpAddr, now: Instant) {
    if state.failed_authentication.len() == MAX_FAILED_AUTHENTICATION_ADDRESSES
        && !state.failed_authentication.contains_key(&address)
        && let Some(oldest) = state
            .failed_authentication
            .iter()
            .min_by_key(|(_, window)| window.started_at)
            .map(|(address, _)| *address)
    {
        state.failed_authentication.remove(&oldest);
    }
    let window = state
        .failed_authentication
        .entry(address)
        .or_insert(FailedAuthenticationWindow {
            started_at: now,
            attempts: 0,
        });
    window.attempts = window.attempts.saturating_add(1);
}

fn persist_catalog(state: &AccessState) -> anyhow::Result<()> {
    let Some(config_path) = &state.config_path else {
        return Ok(());
    };
    let mut root = crate::config::load_configuration_table(config_path)?;
    let mut credentials = state.catalog.credentials.clone();
    for credential in &mut credentials {
        credential.last_used_at_ms = None;
    }
    let persisted = PersistedAccessCatalog {
        version: state.catalog.version,
        credentials,
    };
    root.insert(
        ACCESS_CATALOG_SECTION.to_owned(),
        toml::Value::try_from(persisted)?,
    );
    crate::config::write_configuration_table(config_path, &root)
        .with_context(|| format!("unable to persist {}", config_path.display()))
}

fn load_configured_catalog(config_path: &Path) -> anyhow::Result<Option<AccessCatalog>> {
    let root = crate::config::load_configuration_table(config_path)?;
    let Some(value) = root.get(ACCESS_CATALOG_SECTION) else {
        return Ok(None);
    };
    let persisted = value.clone().try_into::<PersistedAccessCatalog>()?;
    let catalog = AccessCatalog {
        version: persisted.version,
        credentials: persisted.credentials,
        audit: Vec::new(),
    };
    validate_catalog(&catalog)?;
    Ok(Some(catalog))
}

fn load_legacy_catalog(path: &Path) -> anyhow::Result<AccessCatalog> {
    make_file_owner_only(path)?;
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("unable to read {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("unable to parse {}", path.display()))
}

pub fn validate_configuration(root: &toml::Table) -> anyhow::Result<()> {
    let Some(value) = root.get(ACCESS_CATALOG_SECTION) else {
        return Ok(());
    };
    let persisted = value.clone().try_into::<PersistedAccessCatalog>()?;
    validate_catalog(&AccessCatalog {
        version: persisted.version,
        credentials: persisted.credentials,
        audit: Vec::new(),
    })
}

#[cfg(unix)]
fn make_file_owner_only(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
const fn make_file_owner_only(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn bounded_field(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    #[test]
    fn access_key_accepts_uuid_and_reserved_zero() {
        let key: AccessKey = toml::from_str("value = '550e8400-e29b-41d4-a716-446655440000'")
            .and_then(|table: toml::Table| table["value"].clone().try_into())
            .unwrap();
        assert_eq!(key.canonical(), "550e8400-e29b-41d4-a716-446655440000");

        let unset: AccessKey = toml::Value::Integer(0).try_into().unwrap();
        assert!(unset.is_unset());
        assert_eq!(
            toml::Value::try_from(unset).unwrap(),
            toml::Value::Integer(0)
        );
    }

    #[test]
    fn access_key_debug_output_is_redacted() {
        let key = AccessKey::parse("550e8400-e29b-41d4-a716-446655440000").unwrap();
        assert_eq!(format!("{key:?}"), "AccessKey([redacted])");
        assert_ne!(key.fingerprint(), AccessKey::generate().fingerprint());
    }

    #[test]
    fn network_policy_validates_direct_and_proxy_address_fixtures() {
        let policy = NetworkAccessPolicy::new(
            default_local_networks(),
            vec![
                "192.0.2.0/24".parse().unwrap(),
                "2001:db8:1::/48".parse().unwrap(),
            ],
        );
        for address in [
            "127.0.0.1:1",
            "10.0.0.1:1",
            "172.17.0.1:1",
            "192.168.1.1:1",
            "169.254.1.1:1",
            "[::1]:1",
            "[fd00::1]:1",
            "[fe80::1]:1",
            "[::ffff:192.168.1.1]:1",
        ] {
            let peer = address.parse::<SocketAddr>().unwrap().ip();
            assert!(policy.classify(peer, []).local, "{address} must be local");
        }
        for address in ["203.0.113.7:1", "[2001:db8::7]:1", "100.64.0.1:1"] {
            let peer = address.parse::<SocketAddr>().unwrap().ip();
            assert!(!policy.classify(peer, []).local, "{address} must be remote");
        }

        let trusted = "192.0.2.10".parse().unwrap();
        let local = policy.classify(trusted, [("X-Forwarded-For", "192.168.1.50")]);
        assert!(local.local);
        assert_eq!(local.reason, ClientClassificationReason::TrustedProxyLocal);
        let spoofed = policy.classify(trusted, [("X-Forwarded-For", "192.168.1.50, 203.0.113.7")]);
        assert!(!spoofed.local);
        assert_eq!(
            spoofed.effective_address,
            "203.0.113.7".parse::<IpAddr>().unwrap()
        );
        let untrusted = policy.classify(
            "192.168.1.10".parse().unwrap(),
            [("X-Forwarded-For", "192.168.1.50")],
        );
        assert!(!untrusted.local);
        assert_eq!(
            untrusted.reason,
            ClientClassificationReason::UntrustedForwarding
        );
    }

    #[test]
    fn network_policy_rejects_ambiguous_and_malformed_forwarding() {
        let policy = NetworkAccessPolicy::new(
            default_local_networks(),
            vec!["192.0.2.10/32".parse().unwrap()],
        );
        let trusted = "192.0.2.10".parse().unwrap();
        for headers in [
            vec![],
            vec![("X-Forwarded-For", "")],
            vec![("X-Forwarded-For", "192.168.1.2:443")],
            vec![("Forwarded", "for=192.168.1.2")],
            vec![
                ("X-Forwarded-For", "192.168.1.2"),
                ("X-Forwarded-For", "192.168.1.3"),
            ],
        ] {
            assert!(!policy.classify(trusted, headers).local);
        }
    }

    #[test]
    fn credential_lifecycle_persists_in_config_and_audit_resets_on_reopen() {
        let directory = std::env::temp_dir().join(format!("keeppeek-access-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let config_path = directory.join("config.toml");
        std::fs::write(&config_path, "[storage]\nlong_term_max_gb = 10\n").unwrap();
        let initial = AccessKey::parse("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let manager = AccessManager::open(&config_path, initial).unwrap();
        let issued = manager
            .create_credential(
                "Viewer",
                Some("Living room tablet"),
                AccessRole::User,
                Some(20_000),
                10_000,
            )
            .unwrap();
        let config = std::fs::read_to_string(&config_path).unwrap();
        assert!(!config.contains(&issued.access_key.canonical()));
        assert!(config.contains("access_credentials"));
        assert!(config.contains("Viewer"));
        assert!(!directory.join(LEGACY_ACCESS_CATALOG_NAME).exists());

        let authorization = format!("Bearer {}", issued.access_key.canonical());
        let authenticated = manager
            .authenticate(
                "203.0.113.7".parse().unwrap(),
                &[&authorization],
                11_000,
                Instant::now(),
            )
            .unwrap();
        assert_eq!(authenticated.id, issued.metadata.id);
        assert_eq!(authenticated.role, AccessRole::User);

        let disabled = manager
            .set_credential_enabled(issued.metadata.id, false)
            .unwrap();
        assert!(disabled.disabled);
        assert!(
            !std::fs::read_to_string(&config_path)
                .unwrap()
                .contains("last_used_at_ms")
        );
        assert_eq!(
            manager.authenticate(
                "203.0.113.7".parse().unwrap(),
                &[&authorization],
                12_000,
                Instant::now(),
            ),
            Err(AuthenticationFailure::Disabled)
        );
        let reopened = AccessManager::open(&config_path, initial).unwrap();
        assert!(
            reopened
                .list_credentials()
                .iter()
                .any(|credential| credential.id == issued.metadata.id && credential.disabled)
        );
        reopened.record_audit(NewAccessAuditEvent {
            timestamp_ms: 13_000,
            principal_id: Some("local-administrator"),
            role: Some(AccessRole::Administrator),
            action: "credential_disable",
            target_id: Some(&issued.metadata.id.to_string()),
            result: "success",
            client_classification: ClientClassificationReason::DirectLocal,
        });
        reopened.flush_audit(true).unwrap();
        let reopened = AccessManager::open(&config_path, initial).unwrap();
        assert!(reopened.list_audit(10).is_empty());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn failed_authentication_is_rate_limited_per_address() {
        let manager = AccessManager::ephemeral(
            AccessKey::parse("550e8400-e29b-41d4-a716-446655440000").unwrap(),
        );
        manager.configure_rate_limit(2, Duration::from_secs(60));
        let address = "203.0.113.7".parse().unwrap();
        let started = Instant::now();
        assert_eq!(
            manager.authenticate(address, &[], 1_000, started),
            Err(AuthenticationFailure::Missing)
        );
        assert_eq!(
            manager.authenticate(address, &["Bearer invalid"], 1_001, started),
            Err(AuthenticationFailure::Invalid)
        );
        assert_eq!(
            manager.authenticate(address, &[], 1_002, started),
            Err(AuthenticationFailure::RateLimited)
        );
        assert_eq!(
            manager.authenticate(address, &[], 62_000, started + Duration::from_secs(61)),
            Err(AuthenticationFailure::Missing)
        );
    }

    #[test]
    fn initial_key_is_claimed_once_and_rotation_revocation_invalidate_revisions() {
        let initial = AccessKey::parse("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let manager = AccessManager::ephemeral(initial);
        let claimed = manager.claim_initial_access_key(initial).unwrap();
        assert_eq!(claimed.access_key, initial);
        assert!(manager.claim_initial_access_key(initial).is_err());

        let issued = manager
            .create_credential("Operator", None, AccessRole::User, None, 1_000)
            .unwrap();
        let first_revision = issued.metadata.revision;
        let rotated = manager
            .rotate_credential(issued.metadata.id, 2_000)
            .unwrap();
        assert!(rotated.metadata.revision > first_revision);
        assert!(!manager.credential_is_active(issued.metadata.id, first_revision, 2_000));
        assert!(manager.credential_is_active(
            rotated.metadata.id,
            rotated.metadata.revision,
            2_000
        ));

        let revoked = manager
            .revoke_credential(rotated.metadata.id, 3_000)
            .unwrap();
        assert_eq!(revoked.revoked_at_ms, Some(3_000));
        assert!(!manager.credential_is_active(revoked.id, revoked.revision, 3_000));
    }
}
