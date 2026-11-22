use serde::{Deserialize, Serialize};
use std::{fmt, time::Instant};
use uuid::Uuid;

use super::Secret;

pub const MAX_ACTIVE_GRANTS: usize = 256;
pub const MAX_CREATOR_GRANTS: usize = 32;
pub const MAX_GRANT_RECORDS: usize = 4_096;
pub const MAX_GRANT_LIFETIME_MS: i64 = 86_400_000;
pub const AUDIT_RETENTION_MS: i64 = 30 * MAX_GRANT_LIFETIME_MS;
pub const MAX_MEDIA_BYTES: u64 = 512 * 1_024 * 1_024;
pub const MAX_MEDIA_DURATION_MS: i64 = 120_000;
pub const MAX_SESSIONS: usize = 8;
pub const MAX_GRANT_SESSIONS: usize = 2;
pub const SESSION_LIFETIME_MS: i64 = 300_000;
pub const MAX_RECORD_BYTES: usize = 4_096;

/// Uses an injected wall clock for grants and a monotonic clock for session deadlines.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    pub unix_ms: i64,
    pub monotonic: Instant,
}

/// Identifies one immutable, completed export generation and its availability promise.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExportBinding {
    pub job_id: String,
    pub artifact_id: String,
    pub sha256: [u8; 32],
    pub source_id: String,
    pub source_identity: Uuid,
    pub start_ms: i64,
    pub end_ms: i64,
    pub bytes: u64,
    pub available_until_ms: i64,
}

/// Identifies the durable issuer generation; browser sessions are deliberately excluded.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub enum IssuerBinding {
    LocalAdministrator,
    Credential { id: Uuid, revision: u64 },
    ExternalIdentity { id: Uuid, revision: u64 },
}

impl IssuerBinding {
    /// Retains one capacity/listing identity across credential revisions without namespace collisions.
    pub fn creator_id(&self) -> String {
        match self {
            Self::LocalAdministrator => "local-administrator".into(),
            Self::Credential { id, .. } => format!("credential:{id}"),
            Self::ExternalIdentity { id, .. } => format!("external-identity:{id}"),
        }
    }
}
/// The caller must authorize this intent against current creator and source policy.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CreateGrant {
    /// A retry uses the same UUID and returns Conflict without issuing another secret.
    pub request_id: Uuid,
    pub issuer: IssuerBinding,
    pub binding: ExportBinding,
    pub label: String,
    pub note: String,
    pub expires_at_ms: i64,
    pub allow_stream: bool,
    pub allow_download: bool,
    pub max_views: Option<u32>,
    pub max_downloads: Option<u32>,
}

/// Contains no bearer secret or secret hash and is safe for authorized creator listings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantSummary {
    pub id: Uuid,
    pub revision: u64,
    pub intent: CreateGrant,
    pub created_at_ms: i64,
    pub revoked_at_ms: Option<i64>,
    pub revoke_reason: Option<String>,
    pub views: u32,
    pub downloads: u32,
    pub last_access_at_ms: Option<i64>,
}

#[derive(Debug)]
pub struct IssuedGrant {
    pub grant: GrantSummary,
    pub token: Secret,
}

/// This context has no conversion to an authenticated application principal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionContext {
    pub origin: String,
    pub policy_revision: u64,
}

#[derive(Debug)]
pub struct IssuedSession {
    pub id: Uuid,
    pub handle: Secret,
}

/// Carries both expiry clocks so transport cancellation needs no storage lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaDeadline {
    pub expires_at_ms: i64,
    pub monotonic: Instant,
}
/// Authenticates safe status only; terminal receipts cannot authorize media delivery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundSession {
    pub session_id: Uuid,
    pub grant: GrantSummary,
    pub media_active: bool,
    pub media_deadline: Option<MediaDeadline>,
    pub stream_committed: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Stream,
    Download,
}

/// Identifies a counted transfer. Finishing it never refunds its durable consumption.
#[derive(Debug)]
pub struct Ticket {
    pub(super) id: Uuid,
    pub(super) session_id: Uuid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Access {
    pub binding: ExportBinding,
    pub mode: Mode,
    pub expires_at_ms: i64,
}

/// Authentication denial intentionally does not distinguish unknown and inactive grants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Denied,
    Invalid,
    Conflict,
    Capacity,
    Storage,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Denied => "guest access is unavailable",
            Self::Invalid => "invalid guest grant input",
            Self::Conflict => "guest grant request or revision conflicts",
            Self::Capacity => "guest grant capacity reached",
            Self::Storage => "guest grant storage is unavailable",
        })
    }
}

impl std::error::Error for Error {}
