//! Bounded named-volume configuration and deterministic placement decisions.
//!
//! Placement evaluates a supplied capacity snapshot without accessing files or reserving space.
//! A selected volume is a proposal, not permission to write. The writer must reserve capacity
//! and verify the mounted filesystem identity before opening an object.

use serde::{Deserialize, Serialize};
use std::{fmt, path::PathBuf};

mod placement;
pub mod root;
pub(in crate::storage) mod validation;

/// Keeps validation and status responses bounded for a local recorder.
pub const VOLUMES_MAX: usize = 32;
/// Supports per-camera overrides without an unbounded rule search.
pub const RULES_MAX: usize = 256;
/// Limits one placement attempt to a small, explicit destination pool.
pub const CANDIDATES_MAX: usize = 8;

/// Stable configuration identifier, independent of a volume's filesystem path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct VolumeId(String);

impl VolumeId {
    /// Parses a lowercase ASCII identifier containing at most 64 bytes.
    ///
    /// # Errors
    /// Rejects empty, reserved, overlong, or non-identifier input.
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !value.is_empty()
                && value.len() <= 64
                && !value.starts_with("legacy-")
                && value.bytes().all(|byte| byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || byte == b'-'
                    || byte == b'_'),
            "volume ID must contain 1 to 64 lowercase ASCII letters, digits, hyphens or underscores; legacy- is reserved"
        );
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for VolumeId {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<VolumeId> for String {
    fn from(value: VolumeId) -> Self {
        value.0
    }
}

impl fmt::Display for VolumeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Distinguishes data owners when selecting destinations and enforcing capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeRole {
    Active,
    Archive,
    Export,
    Thumbnail,
    Metadata,
}

/// Operator intent; filesystem availability is measured separately.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeState {
    #[default]
    Enabled,
    ReadOnly,
    Draining,
    Disabled,
}

/// Persistent destination settings; roots must not be disclosed to media clients.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Volume<I = VolumeId> {
    pub id: I,
    pub root: PathBuf,
    pub roles: Vec<VolumeRole>,
    #[serde(default)]
    pub state: VolumeState,
    /// Smaller values rank first under priority placement.
    #[serde(default)]
    pub priority: u16,
    /// An omitted cap is unlimited; zero is invalid.
    #[serde(default)]
    pub capacity_bytes: Option<u64>,
    #[serde(default)]
    pub minimum_free_bytes: u64,
    #[serde(default)]
    pub warning_free_bytes: u64,
    #[serde(default)]
    pub critical_free_bytes: u64,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub groups: Vec<String>,
}

impl<I: fmt::Debug> fmt::Debug for Volume<I> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Volume")
            .field("id", &self.id)
            .field("root", &"[REDACTED]")
            .field("roles", &self.roles)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

/// Deterministic ranking within the explicit eligible candidate pool.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementStrategy {
    #[default]
    Priority,
    FreeSpace,
}

/// A source override wins over a group override, which wins over the role default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlacementRule<I = VolumeId> {
    pub role: VolumeRole,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    pub candidates: Vec<I>,
    #[serde(default)]
    pub strategy: PlacementStrategy,
    /// False restricts selection to the first candidate, even if it is unavailable.
    #[serde(default)]
    pub allow_fallback: bool,
}

/// Named destinations and their bounded selection rules, stored together in `config.toml`.
/// String IDs retain secret references in editable drafts; placement requires resolved `VolumeId`s.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, bound(deserialize = "I: Deserialize<'de>"))]
pub struct VolumeConfiguration<I = VolumeId> {
    #[serde(default)]
    pub volumes: Vec<Volume<I>>,
    #[serde(default)]
    pub placement: Vec<PlacementRule<I>>,
}

impl<I> Default for VolumeConfiguration<I> {
    fn default() -> Self {
        Self {
            volumes: Vec::new(),
            placement: Vec::new(),
        }
    }
}

/// Observed mount availability, independent of operator intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeHealth {
    Online,
    Offline,
    ReadOnly,
}

/// Capacity after outstanding reservations have been subtracted by the owning scheduler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeObservation {
    pub id: VolumeId,
    pub health: VolumeHealth,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub owned_bytes: u64,
}

/// A single prospective allocation; it never changes existing object placement.
#[derive(Debug)]
pub struct PlacementRequest<'a> {
    pub role: VolumeRole,
    pub source: &'a str,
    pub group: &'a str,
    pub required_bytes: u64,
}

/// Public rejection categories deliberately contain no filesystem paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectionReason {
    Disabled,
    Draining,
    ReadOnly,
    Offline,
    MissingObservation,
    RoleMismatch,
    SourceDenied,
    InsufficientSpace,
    CapacityExceeded,
}

/// Explains why a candidate cannot receive this prospective allocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedVolume {
    pub id: VolumeId,
    pub reason: RejectionReason,
}

/// A non-mutating proposal; `None` means no eligible destination was proposed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementDecision {
    pub selected: Option<VolumeId>,
    pub rejected: Vec<RejectedVolume>,
}

#[cfg(test)]
mod tests;
