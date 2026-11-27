//! Strictly validated semantic release versions for supervisor A/B slots.
//!
//! Versions use canonical `major.minor.patch` numeric components so they are
//! totally ordered for anti-downgrade floor enforcement and safe to embed as
//! directory names on Windows, macOS, and Linux.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Maximum encoded string length accepted for a release version.
pub const MAX_VERSION_STR_BYTES: usize = 32;

/// Canonical three-part semantic version used to name immutable release slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReleaseVersion {
    /// Major version component.
    pub major: u32,
    /// Minor version component.
    pub minor: u32,
    /// Patch version component.
    pub patch: u32,
}

/// Validation failure returned while parsing a [`ReleaseVersion`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionError {
    /// Input was empty or exceeded [`MAX_VERSION_STR_BYTES`].
    InvalidLength,
    /// Input did not have exactly three dot-separated decimal components.
    InvalidFormat,
    /// A numeric component had a leading zero, non-digit character, or overflow.
    InvalidComponent,
}

impl fmt::Display for VersionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidLength => "release version must be between 1 and 32 ASCII bytes",
            Self::InvalidFormat => "release version must use canonical major.minor.patch format",
            Self::InvalidComponent => {
                "release version components must be canonical unsigned 32-bit integers"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_versions_and_orders_monotonically() {
        let v1 = ReleaseVersion::from_str("0.1.0").unwrap();
        let v2 = ReleaseVersion::from_str("0.2.0").unwrap();
        let v3 = ReleaseVersion::from_str("0.2.10").unwrap();
        let v4 = ReleaseVersion::from_str("1.0.0").unwrap();

        assert!(v1 < v2);
        assert!(v2 < v3);
        assert!(v3 < v4);
        assert_eq!(v3.to_string(), "0.2.10");
    }

    #[test]
    fn rejects_path_traversal_leading_zeros_and_malformed_strings() {
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
