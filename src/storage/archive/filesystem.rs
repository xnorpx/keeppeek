//! Stores exact archive versions under a pinned local filesystem root.

use super::target::{ArchiveTarget, ObjectId, Version, Work};
use crate::storage::volumes::root::{Identity, OwnedFile, Root};
use std::{
    io::{Read, Write},
    path::Path,
};

#[derive(Debug)]
pub struct FilesystemArchiveTarget {
    root: Root,
}

impl FilesystemArchiveTarget {
    pub fn open(path: &Path, expected_root: &Identity) -> anyhow::Result<Self> {
        let root = Root::open(path)?;
        anyhow::ensure!(
            root.identity() == expected_root,
            "archive root identity changed"
        );
        Ok(Self { root })
    }

    /// Read this pinned handle under the owner's reader lease; do not reopen its pathname.
    pub fn open_local(&self, version: &Version, work: &Work<'_>) -> anyhow::Result<OwnedFile> {
        work.check()?;
        version.validate()?;
        let file = self.root.open_owned(
            &format!("{}.mp4", version.object),
            &version.token,
            version.bytes,
        )?;
        work.check()?;
        Ok(file)
    }

    pub(super) fn open_stage(
        &self,
        version: &Version,
        maximum_bytes: u64,
        work: &Work<'_>,
    ) -> anyhow::Result<OwnedFile> {
        work.check()?;
        version.validate()?;
        anyhow::ensure!(
            version.bytes <= maximum_bytes,
            "archive staging bound changed"
        );
        let file = self.root.open_owned_writable(
            &format!("{}.tmp", version.object),
            &version.token,
            0,
            maximum_bytes,
        )?;
        work.check()?;
        Ok(file)
    }

    pub(super) fn confirm_absent(&self, object: ObjectId, work: &Work<'_>) -> anyhow::Result<()> {
        work.check()?;
        self.root
            .confirm_absent(&[&format!("{object}.tmp"), &format!("{object}.mp4")])?;
        work.check()
    }

    pub(super) fn delete_key(
        &self,
        key: &str,
        version: &Version,
        digest: [u8; 32],
        operation: ObjectId,
        work: &Work<'_>,
    ) -> anyhow::Result<()> {
        work.check()?;
        owned_key(key, version)?;
        self.root.retire_owned_until(
            key,
            &version.token,
            version.bytes,
            digest,
            &operation.to_string(),
            work.deadline,
        )?;
        work.check()
    }

    pub(super) fn acknowledge_key(
        &self,
        key: &str,
        version: &Version,
        digest: [u8; 32],
        operation: ObjectId,
        work: &Work<'_>,
    ) -> anyhow::Result<()> {
        work.check()?;
        owned_key(key, version)?;
        self.root.acknowledge_retirement(
            key,
            &version.token,
            version.bytes,
            digest,
            &operation.to_string(),
        )?;
        work.check()
    }

    fn published(
        &self,
        object: ObjectId,
        bytes: u64,
        staged: &Version,
        work: &Work<'_>,
    ) -> anyhow::Result<Option<Version>> {
        let version = Version::new(object, staged.token.clone(), bytes)?;
        match self.open_local(&version, work) {
            Ok(_) => Ok(Some(version)),
            Err(error) if is_missing(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn stage(
        &self,
        object: ObjectId,
        bytes: u64,
        staged: Option<&Version>,
    ) -> anyhow::Result<OwnedFile> {
        let temporary = format!("{object}.tmp");
        match staged {
            Some(version) => self
                .root
                .open_owned_writable(&temporary, &version.token, 0, bytes),
            None => {
                self.root.confirm_absent(&[&format!("{object}.mp4")])?;
                self.root.create_file(&temporary)
            }
        }
    }

    fn checkpoint(
        file: &OwnedFile,
        object: ObjectId,
        callback: &mut dyn FnMut(&Version) -> anyhow::Result<()>,
        work: &Work<'_>,
    ) -> anyhow::Result<Version> {
        let (bytes, token) = file.checkpoint()?;
        let version = Version::new(object, token, bytes)?;
        callback(&version)?;
        work.check()?;
        Ok(version)
    }
}

impl ArchiveTarget for FilesystemArchiveTarget {
    fn publish(
        &self,
        object: ObjectId,
        source: &mut dyn Read,
        bytes: u64,
        staged: Option<&Version>,
        checkpoint: &mut dyn FnMut(&Version) -> anyhow::Result<()>,
        work: &Work<'_>,
    ) -> anyhow::Result<Version> {
        work.check()?;
        anyhow::ensure!(bytes > 0, "archive object must not be empty");
        if let Some(previous) = staged {
            previous.validate()?;
            anyhow::ensure!(
                previous.object == object && previous.bytes <= bytes,
                "archive staging intent changed"
            );
            if let Some(version) = self.published(object, bytes, previous, work)? {
                return Ok(version);
            }
        }
        let mut file = self.stage(object, bytes, staged)?;
        let materialized = file.file_mut().metadata()?.len();
        if let Some(previous) = staged {
            anyhow::ensure!(
                materialized >= previous.bytes,
                "archive staging prefix was truncated"
            );
            verify_prefix(source, &mut file, materialized, work)?;
        }
        Self::checkpoint(&file, object, checkpoint, work)?;
        anyhow::ensure!(
            self.root.capacity(0)?.available_bytes >= bytes - materialized,
            "archive target has insufficient free space"
        );
        copy_staged(
            source,
            &mut file,
            object,
            bytes - materialized,
            checkpoint,
            work,
        )?;
        let version = Self::checkpoint(&file, object, checkpoint, work)?;
        anyhow::ensure!(version.bytes == bytes, "archive staging length changed");
        file.publish_staged(&format!("{object}.mp4"))?;
        self.inspect(&version, work)?;
        Ok(version)
    }

    fn inspect(&self, version: &Version, work: &Work<'_>) -> anyhow::Result<()> {
        drop(self.open_local(version, work)?);
        Ok(())
    }

    fn read(&self, version: &Version, sink: &mut dyn Write, work: &Work<'_>) -> anyhow::Result<()> {
        let mut file = self.open_local(version, work)?;
        let modified = file.file_mut().metadata()?.modified()?;
        copy_exact(&mut file, sink, version.bytes, work)?;
        file.revalidate()?;
        anyhow::ensure!(
            file.file_mut().metadata()?.modified()? == modified,
            "archive changed during readback"
        );
        work.check()
    }

    fn delete(
        &self,
        version: &Version,
        digest: [u8; 32],
        operation: ObjectId,
        work: &Work<'_>,
    ) -> anyhow::Result<()> {
        work.check()?;
        version.validate()?;
        // ponytail: Retirement uses its bounded receipt protocol; cancellation cannot interrupt OS calls.
        self.root.retire_owned_until(
            &format!("{}.mp4", version.object),
            &version.token,
            version.bytes,
            digest,
            &operation.to_string(),
            work.deadline,
        )?;
        work.check()
    }

    fn acknowledge_delete(
        &self,
        version: &Version,
        digest: [u8; 32],
        operation: ObjectId,
        work: &Work<'_>,
    ) -> anyhow::Result<()> {
        work.check()?;
        version.validate()?;
        self.root.acknowledge_retirement(
            &format!("{}.mp4", version.object),
            &version.token,
            version.bytes,
            digest,
            &operation.to_string(),
        )?;
        work.check()
    }
}

fn copy_exact(
    source: &mut dyn Read,
    sink: &mut dyn Write,
    bytes: u64,
    work: &Work<'_>,
) -> anyhow::Result<()> {
    let mut remaining = bytes;
    let mut buffer = [0_u8; 65_536];
    while remaining != 0 {
        work.check()?;
        let amount = usize::try_from(remaining.min(buffer.len() as u64))?;
        source.read_exact(&mut buffer[..amount])?;
        work.check()?;
        sink.write_all(&buffer[..amount])?;
        remaining -= amount as u64;
    }
    work.check()?;
    anyhow::ensure!(
        source.read(&mut buffer[..1])? == 0,
        "archive source exceeds its recorded length"
    );
    work.check()
}

fn copy_staged(
    source: &mut dyn Read,
    file: &mut OwnedFile,
    object: ObjectId,
    bytes: u64,
    checkpoint: &mut dyn FnMut(&Version) -> anyhow::Result<()>,
    work: &Work<'_>,
) -> anyhow::Result<()> {
    let mut remaining = bytes;
    let mut checkpoint_bytes = 0_u64;
    let mut buffer = [0_u8; 65_536];
    while remaining != 0 {
        work.check()?;
        let amount = usize::try_from(remaining.min(buffer.len() as u64))?;
        source.read_exact(&mut buffer[..amount])?;
        work.check()?;
        file.revalidate()?;
        file.file_mut().write_all(&buffer[..amount])?;
        remaining -= amount as u64;
        checkpoint_bytes += amount as u64;
        if checkpoint_bytes >= 8 * 1_048_576 && remaining != 0 {
            FilesystemArchiveTarget::checkpoint(file, object, checkpoint, work)?;
            checkpoint_bytes = 0;
        }
    }
    work.check()?;
    anyhow::ensure!(
        source.read(&mut buffer[..1])? == 0,
        "archive source exceeds its recorded length"
    );
    work.check()
}

fn verify_prefix(
    source: &mut dyn Read,
    file: &mut OwnedFile,
    bytes: u64,
    work: &Work<'_>,
) -> anyhow::Result<()> {
    let mut remaining = bytes;
    let mut expected = [0_u8; 65_536];
    let mut observed = [0_u8; 65_536];
    while remaining != 0 {
        work.check()?;
        let amount = usize::try_from(remaining.min(expected.len() as u64))?;
        source.read_exact(&mut expected[..amount])?;
        work.check()?;
        file.read_exact(&mut observed[..amount])?;
        anyhow::ensure!(
            observed[..amount] == expected[..amount],
            "archive staging prefix differs from source"
        );
        remaining -= amount as u64;
    }
    work.check()
}

fn is_missing(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

fn owned_key(key: &str, version: &Version) -> anyhow::Result<()> {
    version.validate()?;
    anyhow::ensure!(
        key == format!("{}.tmp", version.object) || key == format!("{}.mp4", version.object),
        "archive cleanup key changed"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
