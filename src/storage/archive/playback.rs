//! Verifies only the archive chunks needed for an indexed media range.

use super::{
    CHUNK_BYTES,
    filesystem::FilesystemArchiveTarget,
    target::{ObjectId, Version, Work},
};
use crate::storage::{
    catalog::archive_playback::ArchiveRead,
    volumes::root::{Identity, OwnedFile},
};
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom};

/// Seekable archive reader that verifies each touched chunk before exposing bytes.
pub(crate) struct VerifiedReader<'a> {
    archive: ArchiveRead,
    work: Work<'a>,
    position: u64,
    chunk: Option<(u64, Vec<u8>)>,
}

impl<'a> VerifiedReader<'a> {
    pub(crate) fn new(archive: ArchiveRead, work: Work<'a>) -> anyhow::Result<Self> {
        drop(open_read(&archive, &work)?);
        Ok(Self {
            archive,
            work,
            position: 0,
            chunk: None,
        })
    }

    fn current_chunk(&mut self) -> anyhow::Result<&[u8]> {
        let index = self.position / CHUNK_BYTES;
        if self
            .chunk
            .as_ref()
            .is_none_or(|(cached, _)| *cached != index)
        {
            let start = index * CHUNK_BYTES;
            let length = CHUNK_BYTES.min(self.archive.bytes - start);
            let verified = read_range(&self.archive, start, length, &self.work)?;
            self.chunk = Some((index, verified));
        }
        Ok(&self.chunk.as_ref().expect("verified chunk is present").1)
    }
}

impl Read for VerifiedReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if output.is_empty() || self.position == self.archive.bytes {
            return Ok(0);
        }
        self.work.check().map_err(std::io::Error::other)?;
        let within = usize::try_from(self.position % CHUNK_BYTES).map_err(std::io::Error::other)?;
        let chunk = self.current_chunk().map_err(std::io::Error::other)?;
        let count = output.len().min(chunk.len() - within);
        output[..count].copy_from_slice(&chunk[within..within + count]);
        self.position += u64::try_from(count).map_err(std::io::Error::other)?;
        Ok(count)
    }
}

impl Seek for VerifiedReader<'_> {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.work.check().map_err(std::io::Error::other)?;
        let next = match position {
            SeekFrom::Start(value) => i128::from(value),
            SeekFrom::Current(delta) => i128::from(self.position) + i128::from(delta),
            SeekFrom::End(delta) => i128::from(self.archive.bytes) + i128::from(delta),
        };
        if next < 0 || next > i128::from(self.archive.bytes) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "archive seek is out of range",
            ));
        }
        self.position = u64::try_from(next).map_err(std::io::Error::other)?;
        Ok(self.position)
    }
}

pub(crate) fn open_read(archive: &ArchiveRead, work: &Work<'_>) -> anyhow::Result<OwnedFile> {
    let target = FilesystemArchiveTarget::open(
        &archive.root,
        &Identity {
            filesystem: archive.filesystem.clone(),
            directory: archive.root_identity.clone(),
        },
    )?;
    let version = Version::new(
        ObjectId::parse(&archive.object_id)?,
        archive.version.clone(),
        archive.bytes,
    )?;
    target.open_local(&version, work)
}

pub(crate) fn read_range(
    archive: &ArchiveRead,
    offset: u64,
    length: u64,
    work: &Work<'_>,
) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(
        length > 0 && length <= 256 * 1_048_576,
        "archive playback range exceeds 256 MiB"
    );
    let end = offset
        .checked_add(length)
        .ok_or_else(|| anyhow::anyhow!("archive range overflow"))?;
    anyhow::ensure!(end <= archive.bytes, "archive range exceeds object length");
    anyhow::ensure!(
        u64::try_from(archive.chunk_hashes.len())? == archive.bytes.div_ceil(CHUNK_BYTES) * 32,
        "archive integrity index is incomplete"
    );
    let mut file = open_read(archive, work)?;
    let mut output = Vec::with_capacity(usize::try_from(length)?);
    // ponytail: One reusable chunk verifies boundary bytes without a whole-file warmup.
    let mut buffer = vec![0_u8; usize::try_from(CHUNK_BYTES.min(archive.bytes))?];
    for chunk in offset / CHUNK_BYTES..end.div_ceil(CHUNK_BYTES) {
        work.check()?;
        let start = chunk * CHUNK_BYTES;
        let count = usize::try_from(CHUNK_BYTES.min(archive.bytes - start))?;
        file.file_mut().seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buffer[..count])?;
        let hash_index = usize::try_from(chunk * 32)?;
        anyhow::ensure!(
            Sha256::digest(&buffer[..count]).as_slice()
                == &archive.chunk_hashes[hash_index..hash_index + 32],
            "archive integrity check failed; retain other copies and retry or restore from a verified target"
        );
        let from = usize::try_from(offset.saturating_sub(start))?;
        let to = usize::try_from((end - start).min(count as u64))?;
        output.extend_from_slice(&buffer[from..to]);
    }
    file.revalidate()?;
    work.check()?;
    anyhow::ensure!(
        output.len() == usize::try_from(length)?,
        "archive range was truncated"
    );
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::archive::target::ArchiveTarget;
    use std::{
        io::{Cursor, Write},
        sync::Arc,
        time::Duration,
    };

    fn fixture(
        bytes: &[u8],
    ) -> anyhow::Result<(std::path::PathBuf, FilesystemArchiveTarget, ArchiveRead)> {
        use crate::storage::long_term::inspection::removal::create_private_directory;
        #[cfg(unix)]
        let base = std::fs::canonicalize(std::env::temp_dir())?;
        #[cfg(windows)]
        let base = std::env::temp_dir();
        let parent = cap_std::fs::Dir::open_ambient_dir(&base, cap_std::ambient_authority())?;
        let name = format!("keeppeek-archive-reader-{}", uuid::Uuid::new_v4());
        drop(create_private_directory(
            &parent,
            std::ffi::OsStr::new(&name),
        )?);
        let path = base.join(name);
        let identity = crate::storage::volumes::root::Root::open(&path)?
            .identity()
            .clone();
        let target = FilesystemArchiveTarget::open(&path, &identity)?;
        let object = ObjectId::random();
        let version = target.publish(
            object,
            &mut Cursor::new(bytes),
            bytes.len() as u64,
            None,
            &mut |_| Ok(()),
            &work(),
        )?;
        let hashes = bytes
            .chunks(usize::try_from(CHUNK_BYTES)?)
            .flat_map(|chunk| Sha256::digest(chunk).to_vec())
            .collect::<Vec<_>>();
        let archive = ArchiveRead {
            target_id: "fixture".into(),
            root: path.clone(),
            filesystem: identity.filesystem,
            root_identity: identity.directory,
            object_id: object.to_string(),
            version: version.token,
            bytes: bytes.len() as u64,
            digest: Sha256::digest(bytes).into(),
            chunk_hashes: Arc::from(hashes),
        };
        Ok((path, target, archive))
    }

    fn work() -> Work<'static> {
        fn never() -> bool {
            false
        }
        Work {
            deadline: std::time::Instant::now() + Duration::from_secs(30),
            cancelled: &never,
        }
    }

    #[test]
    fn verified_reader_checks_touched_chunks_after_same_inode_corruption() -> anyhow::Result<()> {
        let bytes = vec![7_u8; usize::try_from(CHUNK_BYTES + 17)?];
        let (path, target, archive) = fixture(&bytes)?;
        let mut reader = VerifiedReader::new(archive.clone(), work())?;
        let mut prefix = [0_u8; 4];
        reader.read_exact(&mut prefix)?;
        assert_eq!(prefix, [7; 4]);
        let object_path = archive.path();
        let mut mutation = std::fs::OpenOptions::new().write(true).open(&object_path)?;
        mutation.seek(SeekFrom::Start(CHUNK_BYTES))?;
        mutation.write_all(&[8])?;
        mutation.sync_all()?;
        drop(mutation);
        reader.seek(SeekFrom::Start(CHUNK_BYTES))?;
        assert!(reader.read_exact(&mut prefix).is_err());
        let mut unaffected = VerifiedReader::new(archive.clone(), work())?;
        unaffected.read_exact(&mut prefix)?;
        assert_eq!(prefix, [7; 4]);
        assert!(unaffected.seek(SeekFrom::End(1)).is_err());
        drop(unaffected);
        drop(reader);
        drop(target);
        std::fs::remove_dir_all(path)?;
        Ok(())
    }

    #[test]
    fn indexed_range_over_32_mib_reads_only_verified_chunks() -> anyhow::Result<()> {
        let bytes = vec![13_u8; 33 * 1_048_576 + 4];
        let (path, target, archive) = fixture(&bytes)?;
        let range = read_range(&archive, 4, 33 * 1_048_576, &work())?;
        assert_eq!(range.len(), 33 * 1_048_576);
        assert!(range.iter().all(|byte| *byte == 13));
        let mut reader = VerifiedReader::new(archive, work())?;
        reader.seek(SeekFrom::Start(32 * 1_048_576 + 7))?;
        let mut tail = [0_u8; 8];
        reader.read_exact(&mut tail)?;
        assert_eq!(tail, [13; 8]);
        drop(reader);
        drop(target);
        std::fs::remove_dir_all(path)?;
        Ok(())
    }
}
