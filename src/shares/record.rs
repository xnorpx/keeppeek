use super::model::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    pub intent: CreateGrant,
    pub revision: u64,
    pub created_at_ms: i64,
    pub revoked_at_ms: Option<i64>,
    pub revoke_reason: Option<String>,
    pub views: u32,
    pub downloads: u32,
    pub last_access_at_ms: Option<i64>,
}

impl Record {
    pub fn summary(&self) -> GrantSummary {
        GrantSummary {
            id: self.intent.request_id,
            revision: self.revision,
            intent: self.intent.clone(),
            created_at_ms: self.created_at_ms,
            revoked_at_ms: self.revoked_at_ms,
            revoke_reason: self.revoke_reason.clone(),
            views: self.views,
            downloads: self.downloads,
            last_access_at_ms: self.last_access_at_ms,
        }
    }

    pub const fn active(&self, now_ms: i64) -> bool {
        self.revoked_at_ms.is_none() && now_ms < self.intent.expires_at_ms
    }

    pub fn validate(&self) -> Result<(), Error> {
        validate_intent(&self.intent, self.created_at_ms).map_err(|_| Error::Storage)?;
        let expected_revision = if self.revoked_at_ms.is_some() { 2 } else { 1 };
        let valid = self.revision == expected_revision
            && self.revoked_at_ms.is_some() == self.revoke_reason.is_some()
            && self
                .revoked_at_ms
                .is_none_or(|time| time >= self.created_at_ms)
            && self
                .revoke_reason
                .as_deref()
                .is_none_or(|reason| bounded_text(reason, 256, false))
            && self
                .last_access_at_ms
                .is_none_or(|time| time >= self.created_at_ms && time < self.intent.expires_at_ms)
            && self
                .intent
                .max_views
                .is_none_or(|limit| self.views <= limit)
            && self
                .intent
                .max_downloads
                .is_none_or(|limit| self.downloads <= limit)
            && (self.intent.allow_stream || self.views == 0)
            && (self.intent.allow_download || self.downloads == 0);
        if valid { Ok(()) } else { Err(Error::Storage) }
    }
}

pub(super) fn validate_intent(intent: &CreateGrant, now_ms: i64) -> Result<(), Error> {
    let binding = &intent.binding;
    let lifetime = intent
        .expires_at_ms
        .checked_sub(now_ms)
        .ok_or(Error::Invalid)?;
    let duration = binding
        .end_ms
        .checked_sub(binding.start_ms)
        .ok_or(Error::Invalid)?;
    let valid = now_ms >= 0
        && !intent.request_id.is_nil()
        && valid_issuer(&intent.issuer)
        && export_id(&binding.job_id)
        && binding.artifact_id.len() <= 36
        && uuid::Uuid::parse_str(&binding.artifact_id).is_ok()
        && bounded_text(&binding.source_id, 256, false)
        && !binding.source_identity.is_nil()
        && binding.start_ms >= 0
        && (1..=MAX_MEDIA_DURATION_MS).contains(&duration)
        && (1..=MAX_MEDIA_BYTES).contains(&binding.bytes)
        && (1..=MAX_GRANT_LIFETIME_MS).contains(&lifetime)
        && intent.expires_at_ms <= binding.available_until_ms
        && bounded_text(&intent.label, 128, false)
        && bounded_text(&intent.note, 512, true)
        && (intent.allow_stream || intent.allow_download)
        && intent.max_views != Some(0)
        && intent.max_downloads != Some(0);
    if valid { Ok(()) } else { Err(Error::Invalid) }
}

pub(super) fn bounded_text(text: &str, limit: usize, allow_empty: bool) -> bool {
    (allow_empty || !text.trim().is_empty())
        && text.len() <= limit
        && !text.chars().any(char::is_control)
}

fn export_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !matches!(id, "." | "..")
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

pub(super) const fn valid_issuer(issuer: &IssuerBinding) -> bool {
    match issuer {
        IssuerBinding::LocalAdministrator => true,
        IssuerBinding::Credential { id, revision }
        | IssuerBinding::ExternalIdentity { id, revision } => !id.is_nil() && *revision > 0,
    }
}
