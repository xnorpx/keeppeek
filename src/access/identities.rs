//! Keeps durable external identities separate from bearer credentials and browser sessions.

use super::{
    AccessRole, CameraAccess,
    external::{Grant, subject_fingerprint},
};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fmt};
use uuid::Uuid;

pub const SECTION: &str = "external_identities";
// JIT provisioning cannot grow the configuration without a fixed upper bound.
const IDENTITY_LIMIT: usize = 1_024;
const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Directory {
    version: u32,
    pub records: Vec<Identity>,
}

impl Default for Directory {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            records: Vec::new(),
        }
    }
}

impl fmt::Debug for Directory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IdentityDirectory")
            .field("count", &self.records.len())
            .finish()
    }
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub id: Uuid,
    pub provider_id: String,
    pub subject_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_policy_fingerprint: Option<String>,
    pub display_name: String,
    pub role: AccessRole,
    pub camera_access: CameraAccess,
    pub revision: u64,
    pub enabled: bool,
    pub created_at_ms: i64,
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExternalIdentity")
            .field("id", &self.id)
            .field("revision", &self.revision)
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

pub struct IdentityInput<'a> {
    pub provider_id: &'a str,
    pub namespace: &'a str,
    pub subject: &'a str,
    pub display_name: &'a str,
    pub grant: Grant,
    pub now_ms: i64,
}

#[derive(Clone, Debug)]
pub struct PreparedIdentity {
    identity: Identity,
}

impl PreparedIdentity {
    pub(crate) const fn identity(&self) -> &Identity {
        &self.identity
    }
}

impl Directory {
    pub(crate) fn prepare_authenticated_admission(
        &self,
        input: IdentityInput<'_>,
        provider: &super::external::Provider,
    ) -> anyhow::Result<PreparedIdentity> {
        let mut candidate = self.clone();
        Ok(PreparedIdentity {
            identity: candidate.provision_authenticated(input, provider)?,
        })
    }

    pub(crate) fn provision_authenticated(
        &mut self,
        input: IdentityInput<'_>,
        provider: &super::external::Provider,
    ) -> anyhow::Result<Identity> {
        ensure!(
            input.provider_id == provider.id,
            "admission provider does not match"
        );
        let fingerprint = super::external::admission_policy_fingerprint(provider)?;
        let provisioned = self.provision(input)?;
        let identity = self
            .records
            .iter_mut()
            .find(|identity| identity.id == provisioned.id)
            .expect("provisioned identity is in the directory");
        identity.admission_policy_fingerprint = Some(fingerprint);
        Ok(identity.clone())
    }

    #[cfg(test)]
    pub(crate) fn prepare_admission(
        &self,
        input: IdentityInput<'_>,
    ) -> anyhow::Result<PreparedIdentity> {
        let mut candidate = self.clone();
        Ok(PreparedIdentity {
            identity: candidate.provision(input)?,
        })
    }

    pub(crate) fn admit_prepared(
        &mut self,
        prepared: &PreparedIdentity,
    ) -> anyhow::Result<Identity> {
        let identity = &prepared.identity;
        ensure!(identity.enabled, "prepared identity is disabled");
        let mut candidate = self.clone();
        if let Some(existing) = candidate
            .records
            .iter_mut()
            .find(|existing| existing.subject_fingerprint == identity.subject_fingerprint)
        {
            ensure!(existing.enabled, "external identity is revoked");
            ensure!(
                existing.id == identity.id && existing.created_at_ms == identity.created_at_ms,
                "prepared identity binding changed"
            );
            let grant_changed = existing.role != identity.role
                || existing.camera_access != identity.camera_access
                || existing.provider_id != identity.provider_id;
            let revision = existing
                .revision
                .checked_add(u64::from(grant_changed))
                .context("identity revision exhausted")?;
            ensure!(
                revision == identity.revision,
                "prepared identity revision changed"
            );
            *existing = identity.clone();
        } else {
            ensure!(
                candidate.records.len() < IDENTITY_LIMIT && identity.revision == 1,
                "prepared identity cannot be admitted"
            );
            candidate.records.push(identity.clone());
        }
        candidate.validate()?;
        *self = candidate;
        Ok(identity.clone())
    }

    pub(crate) fn from_root(root: &toml::Table) -> anyhow::Result<Self> {
        let directory = match root.get(SECTION) {
            Some(value) => value
                .clone()
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid external identity records"))?,
            None => Self::default(),
        };
        directory.validate()?;
        Ok(directory)
    }

    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.version == SCHEMA_VERSION,
            "unsupported external identity schema"
        );
        ensure!(
            self.records.len() <= IDENTITY_LIMIT,
            "external identity capacity exceeded"
        );
        let mut ids = HashSet::with_capacity(self.records.len());
        let mut subjects = HashSet::with_capacity(self.records.len());
        for identity in &self.records {
            validate_text(&identity.provider_id, 64)?;
            validate_text(&identity.display_name, 64)?;
            ensure!(
                identity.revision > 0 && !identity.id.is_nil(),
                "invalid identity revision or ID"
            );
            ensure!(
                identity.created_at_ms >= 0,
                "invalid identity creation time"
            );
            ensure!(ids.insert(identity.id), "duplicate external identity ID");
            ensure!(
                identity.subject_fingerprint.len() == 64
                    && identity
                        .subject_fingerprint
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    && subjects.insert(&identity.subject_fingerprint),
                "invalid or duplicate external subject fingerprint"
            );
            validate_grant(identity.role, &identity.camera_access)?;
            ensure!(
                identity
                    .admission_policy_fingerprint
                    .as_ref()
                    .is_none_or(|value| value.len() == 64
                        && value
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))),
                "invalid admission policy fingerprint"
            );
        }
        Ok(())
    }

    pub(crate) fn provision(&mut self, input: IdentityInput<'_>) -> anyhow::Result<Identity> {
        validate_text(input.provider_id, 64)?;
        validate_text(input.namespace, 2_112)?;
        validate_text(input.subject, 256)?;
        validate_text(input.display_name, 64)?;
        validate_grant(input.grant.role, &input.grant.camera_access)?;
        ensure!(input.now_ms >= 0, "invalid identity creation time");
        let fingerprint = subject_fingerprint(input.namespace, input.subject);
        // ponytail: JIT scans at most 1,024 records; index only if login profiling warrants it.
        if let Some(identity) = self
            .records
            .iter_mut()
            .find(|identity| identity.subject_fingerprint == fingerprint)
        {
            ensure!(identity.enabled, "external identity is revoked");
            if identity.role != input.grant.role
                || identity.camera_access != input.grant.camera_access
                || identity.provider_id != input.provider_id
            {
                identity.revision = identity
                    .revision
                    .checked_add(1)
                    .context("identity revision exhausted")?;
            }
            identity.provider_id = input.provider_id.to_owned();
            identity.display_name = input.display_name.to_owned();
            identity.role = input.grant.role;
            identity.camera_access = input.grant.camera_access;
            identity.admission_policy_fingerprint = None;
            return Ok(identity.clone());
        }
        ensure!(
            self.records.len() < IDENTITY_LIMIT,
            "external identity capacity exceeded"
        );
        let identity = Identity {
            id: Uuid::new_v4(),
            provider_id: input.provider_id.to_owned(),
            subject_fingerprint: fingerprint,
            admission_policy_fingerprint: None,
            display_name: input.display_name.to_owned(),
            role: input.grant.role,
            camera_access: input.grant.camera_access,
            revision: 1,
            enabled: true,
            created_at_ms: input.now_ms,
        };
        self.records.push(identity.clone());
        Ok(identity)
    }

    pub(crate) fn active(&self, id: Uuid, revision: u64) -> Option<&Identity> {
        self.records
            .iter()
            .find(|identity| identity.id == id && identity.enabled && identity.revision == revision)
    }

    pub(crate) fn revoke(&mut self, id: Uuid) -> anyhow::Result<()> {
        let identity = self
            .records
            .iter_mut()
            .find(|identity| identity.id == id)
            .context("external identity not found")?;
        if identity.enabled {
            identity.revision = identity
                .revision
                .checked_add(1)
                .context("identity revision exhausted")?;
            identity.enabled = false;
        }
        Ok(())
    }
}

fn validate_grant(role: AccessRole, policy: &CameraAccess) -> anyhow::Result<()> {
    super::external::validate_camera_access(policy)?;
    ensure!(
        role != AccessRole::Administrator || policy.all_cameras,
        "Administrator requires unrestricted camera access"
    );
    Ok(())
}

fn validate_text(value: &str, limit: usize) -> anyhow::Result<()> {
    ensure!(
        !value.trim().is_empty() && value.len() <= limit && !value.chars().any(char::is_control),
        "external identity text is empty, too long, or contains control characters"
    );
    Ok(())
}

#[cfg(test)]
#[path = "identities_tests.rs"]
mod tests;
