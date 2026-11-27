//! Strictly validated semantic release versions, update channels, and content-addressed slot IDs.
//!
//! `ReleaseVersion` uses canonical `major.minor.patch` numeric components for
//! monotonic floor enforcement and major-line compatibility checks.
//! `SlotId` uniquely identifies an immutable on-disk slot using
//! `<channel>-<semver>[+<build_seq>]-<sha256_prefix_16>` so repeated `0.1.0`
//! `main` builds and local developer rebuilds never collide.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Maximum encoded string length accepted for a release version.
pub const MAX_VERSION_STR_BYTES: usize = 32;

/// Maximum encoded string length accepted for a content-addressed slot identifier.
pub const MAX_SLOT_ID_BYTES: usize = 96;

/// Exact lowercase hexadecimal prefix length embedded into every [`SlotId`].
pub const SLOT_DIGEST_PREFIX_HEX_LEN: usize = 16;

const _: () = {
    assert!(MAX_VERSION_STR_BYTES >= 5);
    assert!(MAX_SLOT_ID_BYTES >= 48);
    assert!(SLOT_DIGEST_PREFIX_HEX_LEN == 16);
};

/// Update channel identifying the trust policy and provenance of a staged slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    /// Official semver GitHub Release verified with pinned offline Ed25519 keys.
    Release,
    /// Rolling `main` (`master` prerelease) build isolated from `release` keys.
    Main,
    /// Local developer build staged explicitly via CLI with host `--allow-local` opt-in.
    Local,
}

impl UpdateChannel {
    /// Returns the canonical lowercase channel name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Main => "main",
            Self::Local => "local",
        }
    }

    /// Returns true for channels backed by signed remote release feeds (`release` or `main`).
    #[must_use]
    pub const fn is_remote(self) -> bool {
        matches!(self, Self::Release | Self::Main)
    }
}

impl fmt::Display for UpdateChannel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for UpdateChannel {
    type Err = VersionError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input {
            "release" => Ok(Self::Release),
            "main" => Ok(Self::Main),
            "local" => Ok(Self::Local),
            _ => Err(VersionError::InvalidChannel),
        }
    }
}

/// Canonical three-part semantic version used for floor ordering and major-line checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReleaseVersion {
    /// Major version component.
    pub major: u32,
    /// Minor version component.
    pub minor: u32,
    /// Patch version component.
    pub patch: u32,
}

/// Validation failure returned while parsing a [`ReleaseVersion`], [`UpdateChannel`], or [`SlotId`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionError {
    /// Input was empty or exceeded its maximum byte length.
    InvalidLength,
    /// Input did not match the canonical version or slot grammar.
    InvalidFormat,
    /// A numeric component had a leading zero, non-digit character, or overflow.
    InvalidComponent,
    /// Channel prefix was not one of `release`, `main`, or `local`.
    InvalidChannel,
    /// Embedded SHA-256 prefix was not 16 lowercase hexadecimal characters.
    InvalidDigestPrefix,
}

impl fmt::Display for VersionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidLength => "version or slot identifier length is out of bounds",
            Self::InvalidFormat => {
                "input does not match canonical semver or slot identifier format"
            }
            Self::InvalidComponent => {
                "numeric components must be canonical unsigned integers without leading zeros"
            }
            Self::InvalidChannel => "update channel must be one of `release`, `main`, or `local`",
            Self::InvalidDigestPrefix => {
                "slot digest prefix must be exactly 16 lowercase hexadecimal characters"
            }
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for VersionError {}

impl ReleaseVersion {
    /// Creates a release version from explicit numeric components.
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Returns true when `candidate` stays on the same major line as `self`.
    ///
    /// Pre-1.0 (`0.x.y`) allows API/schema evolution within `0.x` in place; post-1.0
    /// (`>= 1.0.0`) allows additive minor/patch updates in place. Any major change
    /// (`0.x -> 1.0`, `1.x -> 2.0`, etc.) requires updating the supervisor via a fresh install.
    #[must_use]
    pub const fn supports_in_place_upgrade_to(self, candidate: Self) -> bool {
        candidate.major == self.major
    }
}

/// Returns the compiled supervisor semantic version from `Cargo.toml`.
#[must_use]
pub fn current_supervisor_version() -> ReleaseVersion {
    ReleaseVersion::from_str(env!("CARGO_PKG_VERSION"))
        .expect("CARGO_PKG_VERSION must be a canonical major.minor.patch semver")
}

impl fmt::Display for ReleaseVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl FromStr for ReleaseVersion {
    type Err = VersionError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        if input.is_empty() || input.len() > MAX_VERSION_STR_BYTES {
            return Err(VersionError::InvalidLength);
        }
        let mut parts = input.split('.');
        let major_str = parts.next().ok_or(VersionError::InvalidFormat)?;
        let minor_str = parts.next().ok_or(VersionError::InvalidFormat)?;
        let patch_str = parts.next().ok_or(VersionError::InvalidFormat)?;
        if parts.next().is_some() {
            return Err(VersionError::InvalidFormat);
        }
        let major = parse_canonical_u32(major_str)?;
        let minor = parse_canonical_u32(minor_str)?;
        let patch = parse_canonical_u32(patch_str)?;
        Ok(Self::new(major, minor, patch))
    }
}

impl Serialize for ReleaseVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ReleaseVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::from_str(&raw).map_err(serde::de::Error::custom)
    }
}

/// Monotonic floor tuple enforced on the `main` channel where `Cargo.toml` semver may stay flat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MainBuildFloor {
    /// Package semantic version recorded in the manifest.
    pub version: ReleaseVersion,
    /// Monotonic CI build sequence number (for example GitHub Actions `run_number`).
    pub build_seq: u64,
}

impl MainBuildFloor {
    /// Creates a `main` channel floor key from `version` and `build_seq`.
    #[must_use]
    pub const fn new(version: ReleaseVersion, build_seq: u64) -> Self {
        Self { version, build_seq }
    }
}

impl fmt::Display for MainBuildFloor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}+{}", self.version, self.build_seq)
    }
}

/// Validated content-addressed slot identifier safe to use as a directory name on all OSes.
///
/// Format: `<channel>-<semver>[+<build_seq>]-<sha256_prefix_16>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SlotId {
    raw: String,
    channel: UpdateChannel,
    version: ReleaseVersion,
    build_seq: Option<u64>,
}

impl SlotId {
    /// Constructs a canonical [`SlotId`] from channel, semver, optional build sequence, and SHA-256 hex.
    ///
    /// # Errors
    ///
    /// Returns [`VersionError`] if `sha256_hex` lacks a valid 16-character lowercase
    /// hex prefix or if the formatted identifier exceeds [`MAX_SLOT_ID_BYTES`].
    pub fn new(
        channel: UpdateChannel,
        version: ReleaseVersion,
        build_seq: Option<u64>,
        sha256_hex: &str,
    ) -> Result<Self, VersionError> {
        if sha256_hex.len() < SLOT_DIGEST_PREFIX_HEX_LEN {
            return Err(VersionError::InvalidDigestPrefix);
        }
        let prefix = &sha256_hex[..SLOT_DIGEST_PREFIX_HEX_LEN];
        if !is_lowercase_hex(prefix) {
            return Err(VersionError::InvalidDigestPrefix);
        }
        let raw = build_seq.map_or_else(
            || format!("{channel}-{version}-{prefix}"),
            |seq| format!("{channel}-{version}+{seq}-{prefix}"),
        );
        if raw.len() > MAX_SLOT_ID_BYTES {
            return Err(VersionError::InvalidLength);
        }
        debug_assert!(
            raw.bytes().all(is_valid_slot_byte),
            "constructed SlotId must contain only safe directory characters"
        );
        Ok(Self {
            raw,
            channel,
            version,
            build_seq,
        })
    }

    /// Returns the canonical string form used as the slot directory name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Returns the channel encoded in this slot identifier.
    #[must_use]
    pub const fn channel(&self) -> UpdateChannel {
        self.channel
    }

    /// Returns the semantic version encoded in this slot identifier.
    #[must_use]
    pub const fn version(&self) -> ReleaseVersion {
        self.version
    }

    /// Returns the CI build sequence encoded in this slot identifier, if present.
    #[must_use]
    pub const fn build_seq(&self) -> Option<u64> {
        self.build_seq
    }

    /// Returns the 16-character lowercase SHA-256 digest prefix of this slot.
    #[must_use]
    pub fn digest_prefix(&self) -> &str {
        let start = self.raw.len().saturating_sub(SLOT_DIGEST_PREFIX_HEX_LEN);
        &self.raw[start..]
    }
}

impl fmt::Display for SlotId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.raw)
    }
}

impl FromStr for SlotId {
    type Err = VersionError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        if input.is_empty() || input.len() > MAX_SLOT_ID_BYTES {
            return Err(VersionError::InvalidLength);
        }
        if !input.bytes().all(is_valid_slot_byte) || input.contains("..") {
            return Err(VersionError::InvalidFormat);
        }
        let (channel_str, rest) = input.split_once('-').ok_or(VersionError::InvalidFormat)?;
        let channel = UpdateChannel::from_str(channel_str)?;
        let (ver_and_seq, digest_prefix) =
            rest.rsplit_once('-').ok_or(VersionError::InvalidFormat)?;
        if digest_prefix.len() != SLOT_DIGEST_PREFIX_HEX_LEN || !is_lowercase_hex(digest_prefix) {
            return Err(VersionError::InvalidDigestPrefix);
        }
        let (version, build_seq) = match ver_and_seq.split_once('+') {
            Some((ver_str, seq_str)) => (
                ReleaseVersion::from_str(ver_str)?,
                Some(parse_canonical_u64(seq_str)?),
            ),
            None => (ReleaseVersion::from_str(ver_and_seq)?, None),
        };
        let canonical = Self::new(channel, version, build_seq, digest_prefix)?;
        if canonical.raw != input {
            return Err(VersionError::InvalidFormat);
        }
        Ok(canonical)
    }
}

impl Serialize for SlotId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.raw)
    }
}

impl<'de> Deserialize<'de> for SlotId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::from_str(&raw).map_err(serde::de::Error::custom)
    }
}

fn parse_canonical_u32(segment: &str) -> Result<u32, VersionError> {
    if segment.is_empty() || (segment.len() > 1 && segment.starts_with('0')) {
        return Err(VersionError::InvalidComponent);
    }
    if !segment.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(VersionError::InvalidComponent);
    }
    segment
        .parse::<u32>()
        .map_err(|_| VersionError::InvalidComponent)
}

fn parse_canonical_u64(segment: &str) -> Result<u64, VersionError> {
    if segment.is_empty() || (segment.len() > 1 && segment.starts_with('0')) {
        return Err(VersionError::InvalidComponent);
    }
    if !segment.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(VersionError::InvalidComponent);
    }
    segment
        .parse::<u64>()
        .map_err(|_| VersionError::InvalidComponent)
}

fn is_lowercase_hex(input: &str) -> bool {
    input
        .bytes()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

const fn is_valid_slot_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'+' | b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_versions_and_enforces_major_line_rules() {
        let v01 = ReleaseVersion::from_str("0.1.0").unwrap();
        let v02 = ReleaseVersion::from_str("0.2.0").unwrap();
        let v10 = ReleaseVersion::from_str("1.0.0").unwrap();
        let v13 = ReleaseVersion::from_str("1.3.4").unwrap();
        let v20 = ReleaseVersion::from_str("2.0.0").unwrap();

        assert!(v01 < v02 && v02 < v10 && v10 < v13 && v13 < v20);
        assert!(v01.supports_in_place_upgrade_to(v02));
        assert!(!v02.supports_in_place_upgrade_to(v10));
        assert!(v10.supports_in_place_upgrade_to(v13));
        assert!(!v13.supports_in_place_upgrade_to(v20));
    }

    #[test]
    fn parses_and_validates_content_addressed_slot_ids() {
        let release = SlotId::new(
            UpdateChannel::Release,
            ReleaseVersion::new(0, 2, 0),
            None,
            "0123456789abcdefdeadbeef",
        )
        .unwrap();
        assert_eq!(release.as_str(), "release-0.2.0-0123456789abcdef");
        assert_eq!(SlotId::from_str(release.as_str()).unwrap(), release);

        let main = SlotId::new(
            UpdateChannel::Main,
            ReleaseVersion::new(0, 1, 0),
            Some(142),
            "abcdef012345678900000000",
        )
        .unwrap();
        assert_eq!(main.as_str(), "main-0.1.0+142-abcdef0123456789");
        assert_eq!(main.build_seq(), Some(142));
        assert_eq!(SlotId::from_str(main.as_str()).unwrap(), main);

        for invalid in [
            "",
            "release-0.1.0",
            "unknown-0.1.0-0123456789abcdef",
            "local-0.1.0-0123456789ABCDEF",
            "local-0.1.0-short",
            "local-../0.1.0-0123456789abcdef",
            "local-0.1.0+01-0123456789abcdef",
            "local-0.1.0-0123456789abcdef/evil",
        ] {
            assert!(
                SlotId::from_str(invalid).is_err(),
                "expected invalid SlotId to fail: {invalid}"
            );
        }
    }

    #[test]
    fn rejects_path_traversal_leading_zeros_and_malformed_versions() {
        for invalid in [
            "",
            "1.0",
            "1.0.0.0",
            "v1.0.0",
            "01.0.0",
            "1.00.0",
            "1.0.0-beta",
            "../1.0.0",
            "1.0.0/evil",
            "1.0.0\\evil",
            "999999999999999999999999999999999",
        ] {
            assert!(
                ReleaseVersion::from_str(invalid).is_err(),
                "expected invalid version to fail: {invalid}"
            );
        }
    }
}
