//! Defines immutable archive object operations outside the recording path.

use serde::{Deserialize, Serialize};
use std::{
    fmt,
    io::{Read, Write},
    time::Instant,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ObjectId(uuid::Uuid);

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Version {
    pub object: ObjectId,
    pub token: String,
    pub bytes: u64,
}

pub struct Work<'a> {
    pub deadline: Instant,
    pub cancelled: &'a dyn Fn() -> bool,
}

impl ObjectId {
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        let id = uuid::Uuid::parse_str(value)?;
        anyhow::ensure!(
            id.to_string() == value,
            "archive object ID is not canonical"
        );
        Ok(Self(id))
    }

    pub fn random() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Version {
    pub fn new(object: ObjectId, token: String, bytes: u64) -> anyhow::Result<Self> {
        let version = Self {
            object,
            token,
            bytes,
        };
        version.validate()?;
        Ok(version)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.token.is_empty()
                && self.token.len() <= 256
                && !self.token.chars().any(char::is_control),
            "archive version token is invalid"
        );
        Ok(())
    }
}

impl fmt::Debug for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Version")
            .field("object", &self.object)
            .field("bytes", &self.bytes)
            .finish_non_exhaustive()
    }
}

impl Work<'_> {
    pub fn check(&self) -> anyhow::Result<()> {
        anyhow::ensure!(!(self.cancelled)(), "archive operation was cancelled");
        anyhow::ensure!(
            Instant::now() < self.deadline,
            "archive operation timed out"
        );
        Ok(())
    }
}

impl fmt::Debug for Work<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Work")
            .field("deadline", &self.deadline)
            .finish_non_exhaustive()
    }
}

/// Call these blocking operations only from bounded archive workers.
/// The owner retains source and reader leases and verifies the independent readback.
pub trait ArchiveTarget {
    /// Persist each checkpoint before returning from the callback.
    /// Recovery must supply that exact evidence and a source reader at offset zero.
    fn publish(
        &self,
        object: ObjectId,
        source: &mut dyn Read,
        bytes: u64,
        staged: Option<&Version>,
        checkpoint: &mut dyn FnMut(&Version) -> anyhow::Result<()>,
        work: &Work<'_>,
    ) -> anyhow::Result<Version>;

    fn inspect(&self, version: &Version, work: &Work<'_>) -> anyhow::Result<()>;

    fn read(&self, version: &Version, sink: &mut dyn Write, work: &Work<'_>) -> anyhow::Result<()>;

    /// The owner must authorize this exact version after checking holds and readers.
    fn delete(
        &self,
        version: &Version,
        digest: [u8; 32],
        operation: ObjectId,
        work: &Work<'_>,
    ) -> anyhow::Result<()>;

    /// Call only after the owner durably records the successful deletion.
    fn acknowledge_delete(
        &self,
        version: &Version,
        digest: [u8; 32],
        operation: ObjectId,
        work: &Work<'_>,
    ) -> anyhow::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_ids_and_versions_reject_invalid_boundary_values() -> anyhow::Result<()> {
        let id = ObjectId::random();
        assert_eq!(ObjectId::parse(&id.to_string())?, id);
        for value in ["../outside", "CON", "00000000000000000000000000000000"] {
            assert!(ObjectId::parse(value).is_err());
        }
        for token in [String::new(), "x".repeat(257), "line\nbreak".to_owned()] {
            assert!(Version::new(id, token, 1).is_err());
        }
        let version = Version::new(id, "private-version".to_owned(), 1)?;
        assert!(!format!("{version:?}").contains("private-version"));
        Ok(())
    }
}
