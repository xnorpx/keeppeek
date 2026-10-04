use super::Root;
use crate::storage::long_term::inspection::removal::{sync_directory, validate_owner};
use cap_fs_ext::{FollowSymlinks, MetadataExt, OpenOptionsFollowExt};
use cap_std::fs::OpenOptions;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    time::{Duration, Instant},
};

/// An owned leaf whose parent and file handles remain pinned.
pub struct OwnedFile {
    root: Root,
    key: String,
    file: File,
    identity: String,
    expected_bytes: Option<u64>,
}

impl std::fmt::Debug for OwnedFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OwnedFile([REDACTED])")
    }
}

impl Root {
    /// Creates a UUID-named leaf without overwriting or following an existing entry.
    ///
    /// # Errors
    /// Rejects unsafe names, permissions, replaced roots, and unsupported durability.
    pub fn create_file(&self, key: &str) -> anyhow::Result<OwnedFile> {
        validate_key(key)?;
        self.revalidate()?;
        validate_owner(&self.directory, 0o022)?;
        sync_directory(&self.directory)?;
        let mut options = file_options();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        #[cfg(windows)]
        {
            use cap_std::fs::OpenOptionsExt;
            use windows::Win32::{
                Foundation::{GENERIC_READ, GENERIC_WRITE},
                Storage::FileSystem::{DELETE, FILE_SHARE_READ},
            };
            options
                .access_mode(GENERIC_READ.0 | GENERIC_WRITE.0 | DELETE.0)
                .share_mode(FILE_SHARE_READ.0);
        }
        let file = self.directory.open_with(key, &options)?.into_std();
        let identity = file_identity(&file)?;
        let owned = OwnedFile {
            root: Self {
                path: self.path.clone(),
                directory: self.directory.try_clone()?,
                identity: self.identity.clone(),
            },
            key: key.to_owned(),
            file,
            identity,
            expected_bytes: None,
        };
        owned.revalidate()?;
        owned.file.sync_all()?;
        owned.root.sync()?;
        Ok(owned)
    }

    /// Reopens a journal-owned leaf for reading without adopting or changing its contents.
    ///
    /// # Errors
    /// Rejects unsafe names, links, replaced roots, or mismatched identity and length.
    pub fn open_owned(
        &self,
        key: &str,
        expected_identity: &str,
        expected_bytes: u64,
    ) -> anyhow::Result<OwnedFile> {
        validate_key(key)?;
        self.revalidate()?;
        validate_owner(&self.directory, 0o022)?;
        let mut options = file_options();
        #[cfg(windows)]
        {
            use cap_std::fs::OpenOptionsExt;
            use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;
            options.share_mode(FILE_SHARE_READ.0);
        }
        options.write(false);
        let file = self.directory.open_with(key, &options)?.into_std();
        anyhow::ensure!(
            file_identity(&file)? == expected_identity,
            "owned file identity changed"
        );
        let owned = OwnedFile {
            root: Self {
                path: self.path.clone(),
                directory: self.directory.try_clone()?,
                identity: self.identity.clone(),
            },
            key: key.to_owned(),
            file,
            identity: expected_identity.to_owned(),
            expected_bytes: Some(expected_bytes),
        };
        owned.revalidate()?;
        Ok(owned)
    }

    /// Synchronizes the pinned directory after validating its name and permissions.
    ///
    /// # Errors
    /// Rejects changed roots, unsafe permissions, or failed directory synchronization.
    pub fn sync(&self) -> anyhow::Result<()> {
        self.revalidate()?;
        validate_owner(&self.directory, 0o022)?;
        sync_directory(&self.directory)?;
        self.revalidate()
    }

    /// Reopens a journal-owned file for recovery without creating or truncating it.
    /// The caller must retain its capacity reservation and exclusive worker lease.
    ///
    /// # Errors
    /// Rejects changed identity, unsafe roots, links, or a length outside the reserved range.
    pub(crate) fn open_owned_writable(
        &self,
        key: &str,
        expected_identity: &str,
        minimum_bytes: u64,
        maximum_bytes: u64,
    ) -> anyhow::Result<OwnedFile> {
        validate_key(key)?;
        anyhow::ensure!(
            (key.ends_with(".tmp") || key.ends_with(".mp4")) && minimum_bytes <= maximum_bytes,
            "invalid writable recovery range"
        );
        self.revalidate()?;
        validate_owner(&self.directory, 0o022)?;
        let mut options = file_options();
        options.write(true);
        #[cfg(windows)]
        {
            use cap_std::fs::OpenOptionsExt;
            use windows::Win32::{
                Foundation::{GENERIC_READ, GENERIC_WRITE},
                Storage::FileSystem::{DELETE, FILE_SHARE_READ},
            };
            options
                .access_mode(GENERIC_READ.0 | GENERIC_WRITE.0 | DELETE.0)
                .share_mode(FILE_SHARE_READ.0);
        }
        let file = self.directory.open_with(key, &options)?.into_std();
        anyhow::ensure!(
            file_identity(&file)? == expected_identity,
            "owned file identity changed"
        );
        let bytes = file.metadata()?.len();
        anyhow::ensure!(
            (minimum_bytes..=maximum_bytes).contains(&bytes),
            "recovery file length is outside its reservation"
        );
        let owned = OwnedFile {
            root: Self {
                path: self.path.clone(),
                directory: self.directory.try_clone()?,
                identity: self.identity.clone(),
            },
            key: key.to_owned(),
            file,
            identity: expected_identity.to_owned(),
            expected_bytes: None,
        };
        owned.revalidate()?;
        Ok(owned)
    }
}

impl OwnedFile {
    /// Publishes staged media under a new UUID name without replacing another entry.
    /// The writable handle closes on success and failure; errors never trigger file deletion.
    ///
    /// # Errors
    /// Rejects read-only handles, conflicting names, changed roots, or failed synchronization.
    /// A failure after rename requires journal recovery to inspect the destination.
    pub fn publish_staged(mut self, target: &str) -> anyhow::Result<()> {
        validate_key(target)?;
        anyhow::ensure!(
            (target.ends_with(".mp4") || target.ends_with(".jpg")) && target != self.key,
            "publication requires a new media name"
        );
        anyhow::ensure!(
            self.expected_bytes.is_none(),
            "read-only files cannot publish names"
        );
        self.revalidate()?;
        self.file.sync_all()?;
        self.root.sync()?;
        #[cfg(windows)]
        crate::storage::long_term::inspection::removal::windows::rename_to(
            &self.file,
            &self.root.directory,
            std::ffi::OsStr::new(target),
        )?;
        #[cfg(unix)]
        crate::storage::long_term::inspection::removal::unix::rename_to(
            &self.root.directory,
            std::ffi::OsStr::new(&self.key),
            &self.root.directory,
            std::ffi::OsStr::new(target),
        )?;
        #[cfg(not(any(unix, windows)))]
        anyhow::bail!("owned publication is unsupported on this platform");
        self.key = target.to_owned();
        self.revalidate()?;
        self.root.sync()?;
        self.revalidate()
    }

    /// Synchronizes written bytes before releasing their physical-space reservation.
    ///
    /// # Errors
    /// Rejects replaced files or unsuccessful synchronization.
    pub(crate) fn checkpoint(&self) -> anyhow::Result<(u64, String)> {
        self.revalidate()?;
        self.file.sync_data()?;
        let bytes = self.file.metadata()?.len();
        self.revalidate()?;
        Ok((bytes, self.identity.clone()))
    }

    /// Borrows the pinned handle; the caller must reserve capacity before writing.
    pub const fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    /// Synchronizes and hashes the pinned handle, preserving its current cursor.
    /// The sixty-second deadline is cooperative and cannot interrupt filesystem calls.
    ///
    /// # Errors
    /// Rejects replacement, extra links, concurrent mutation, or failed synchronization.
    pub fn evidence(&mut self) -> anyhow::Result<(u64, String, [u8; 32])> {
        self.evidence_with_progress(|_| Ok(()))
    }

    pub(crate) fn evidence_with_progress(
        &mut self,
        mut progress: impl FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<(u64, String, [u8; 32])> {
        let deadline = Instant::now() + Duration::from_secs(60);
        anyhow::ensure!(
            self.expected_bytes.is_none(),
            "read-only files cannot establish write durability"
        );
        self.revalidate()?;
        progress(0)?;
        self.file.sync_all()?;
        self.root.sync()?;
        self.inspect_until(deadline, &mut progress)
    }

    /// Hashes the pinned file without establishing write durability, preserving its cursor.
    /// The sixty-second deadline is cooperative and cannot interrupt filesystem calls.
    ///
    /// # Errors
    /// Rejects replacement, changed length, extra links, or mutation during verification.
    pub fn inspect_evidence(&mut self) -> anyhow::Result<(u64, String, [u8; 32])> {
        self.inspect_until(Instant::now() + Duration::from_secs(60), &mut |_| Ok(()))
    }

    fn inspect_until(
        &mut self,
        deadline: Instant,
        progress: &mut impl FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<(u64, String, [u8; 32])> {
        self.revalidate()?;
        let before = self.file.metadata()?;
        let position = self.file.stream_position()?;
        self.file.rewind()?;
        let digest = hash_with_progress(&mut self.file, before.len(), deadline, progress);
        self.file.seek(SeekFrom::Start(position))?;
        let digest = digest?;
        self.revalidate()?;
        let after = self.file.metadata()?;
        anyhow::ensure!(
            before.len() == after.len() && before.modified()? == after.modified()?,
            "owned file changed during verification"
        );
        anyhow::ensure!(Instant::now() < deadline, "file verification timed out");
        Ok((after.len(), self.identity.clone(), digest))
    }

    /// Checks the pinned file and its name before another reserved write.
    ///
    /// # Errors
    /// Rejects changed roots, unsafe permissions, links, or replaced files.
    pub(crate) fn revalidate(&self) -> anyhow::Result<()> {
        self.root.revalidate()?;
        validate_owner(&self.root.directory, 0o022)?;
        anyhow::ensure!(
            file_identity(&self.file)? == self.identity,
            "owned file identity changed"
        );
        if let Some(expected_bytes) = self.expected_bytes {
            anyhow::ensure!(
                self.file.metadata()?.len() == expected_bytes,
                "owned file length changed"
            );
        }
        let named = self
            .root
            .directory
            .open_with(&self.key, &file_options())?
            .into_std();
        anyhow::ensure!(
            file_identity(&named)? == self.identity,
            "owned file name changed"
        );
        Ok(())
    }
}

impl Read for OwnedFile {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.revalidate().map_err(std::io::Error::other)?;
        let amount = buffer.len().min(65_536);
        let read = self.file.read(&mut buffer[..amount])?;
        self.revalidate().map_err(std::io::Error::other)?;
        Ok(read)
    }
}

pub(super) fn validate_key(key: &str) -> anyhow::Result<()> {
    let (stem, extension) = key
        .split_once('.')
        .ok_or_else(|| anyhow::anyhow!("invalid object key"))?;
    anyhow::ensure!(
        (stem.len() == 32 || stem.len() == 36)
            && stem
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
            && uuid::Uuid::parse_str(stem).is_ok()
            && matches!(extension, "mp4" | "jpg" | "webp" | "png" | "tmp"),
        "invalid object key"
    );
    Ok(())
}

pub(super) fn file_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_fs_ext::OpenOptionsSyncExt;
        options.nonblock(true);
    }
    options
}

pub(super) fn hash(file: &mut File, bytes: u64, deadline: Instant) -> anyhow::Result<[u8; 32]> {
    hash_with_progress(file, bytes, deadline, &mut |_| Ok(()))
}

fn hash_with_progress(
    file: &mut File,
    bytes: u64,
    deadline: Instant,
    progress: &mut impl FnMut(u64) -> anyhow::Result<()>,
) -> anyhow::Result<[u8; 32]> {
    let mut remaining = bytes;
    let mut hasher = Sha256::new();
    // ponytail: A fixed buffer and initial length bound memory and stop concurrent growth.
    let mut buffer = [0_u8; 65_536];
    while remaining != 0 {
        anyhow::ensure!(Instant::now() < deadline, "file verification timed out");
        let amount = usize::try_from(remaining.min(buffer.len() as u64))?;
        file.read_exact(&mut buffer[..amount])?;
        hasher.update(&buffer[..amount]);
        remaining -= amount as u64;
        progress(bytes - remaining)?;
    }
    Ok(hasher.finalize().into())
}

pub(super) fn file_identity(file: &File) -> anyhow::Result<String> {
    let metadata = cap_std::fs::File::from_std(file.try_clone()?).metadata()?;
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.nlink() == 1,
        "owned leaf must be a regular file with one link"
    );
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{
            Foundation::HANDLE,
            Storage::FileSystem::{
                FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FILE_ID_INFO,
                FileAttributeTagInfo, FileIdInfo, GetFileInformationByHandleEx,
            },
        };
        let mut info = FILE_ID_INFO::default();
        let mut attributes = FILE_ATTRIBUTE_TAG_INFO::default();
        // SAFETY: The pinned handle is valid and both outputs have the requested sizes.
        unsafe {
            GetFileInformationByHandleEx(
                HANDLE(file.as_raw_handle()),
                FileIdInfo,
                std::ptr::from_mut(&mut info).cast(),
                u32::try_from(size_of::<FILE_ID_INFO>())?,
            )?;
            GetFileInformationByHandleEx(
                HANDLE(file.as_raw_handle()),
                FileAttributeTagInfo,
                std::ptr::from_mut(&mut attributes).cast(),
                u32::try_from(size_of::<FILE_ATTRIBUTE_TAG_INFO>())?,
            )?;
        }
        anyhow::ensure!(
            attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0,
            "owned leaf cannot be a reparse point"
        );
        Ok(format!(
            "{}:{:032x}",
            info.VolumeSerialNumber,
            u128::from_be_bytes(info.FileId.Identifier)
        ))
    }
    #[cfg(unix)]
    {
        Ok(format!("{}:{}", metadata.dev(), metadata.ino()))
    }
    #[cfg(not(any(unix, windows)))]
    {
        anyhow::bail!("owned file identity is unsupported")
    }
}

#[cfg(test)]
mod reopen_tests;

#[cfg(test)]
mod publish_tests;

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    pub(in crate::storage::volumes::root) fn fixture() -> anyhow::Result<(PathBuf, Root)> {
        let base = std::env::temp_dir();
        #[cfg(unix)]
        let base = std::fs::canonicalize(base)?;
        let path = base.join(format!("keeppeek-owned-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path)?;
        #[cfg(windows)]
        anyhow::ensure!(
            std::process::Command::new("powershell.exe")
                .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"])
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/.github/scripts/protect-test-directory.ps1"
                ))
                .arg("-Directory")
                .arg(&path)
                .status()?
                .success(),
            "cannot protect fixture directory"
        );
        Ok((path.clone(), Root::open(&path)?))
    }

    #[test]
    fn evidence_hashes_the_pinned_file_and_preserves_cursor() -> anyhow::Result<()> {
        let (path, root) = fixture()?;
        let key = format!("{}.mp4", uuid::Uuid::new_v4());
        let mut owned = root.create_file(&key)?;
        let mut contents = vec![42_u8; 65_536];
        contents.extend_from_slice(b"abc");
        owned.file_mut().write_all(&contents)?;
        owned.file_mut().seek(SeekFrom::Start(1))?;
        let (bytes, identity, digest) = owned.evidence()?;
        assert_eq!(bytes, 65_539);
        assert!(!identity.is_empty());
        assert_eq!(digest, <[u8; 32]>::from(Sha256::digest(&contents)));
        assert_eq!(owned.file_mut().stream_position()?, 1);
        assert!(root.create_file(&key).is_err());
        assert_eq!(std::fs::read(path.join(key))?, contents);
        Ok(())
    }

    #[test]
    fn unsafe_names_and_existing_directories_are_rejected() -> anyhow::Result<()> {
        let (path, root) = fixture()?;
        for key in [
            "../outside.mp4",
            "plain.mp4",
            "CON.mp4",
            ".hidden",
            "x/y.mp4",
        ] {
            assert!(root.create_file(key).is_err());
        }
        let key = format!("{}.mp4", uuid::Uuid::new_v4());
        std::fs::create_dir(path.join(&key))?;
        assert!(root.create_file(&key).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn evidence_rejects_replaced_leaf_multiple_links_and_root() -> anyhow::Result<()> {
        let (path, root) = fixture()?;
        let key = format!("{}.mp4", uuid::Uuid::new_v4());
        let mut owned = root.create_file(&key)?;
        owned.file_mut().write_all(b"original")?;
        std::fs::hard_link(path.join(&key), path.join("alias"))?;
        assert!(owned.evidence().is_err());
        std::fs::remove_file(path.join("alias"))?;
        std::fs::rename(path.join(&key), path.join("retained"))?;
        std::fs::write(path.join(&key), b"substitute")?;
        assert!(owned.evidence().is_err());
        let second_key = format!("{}.mp4", uuid::Uuid::new_v4());
        let mut second = root.create_file(&second_key)?;
        let retained = path.with_extension("retained");
        std::fs::rename(&path, &retained)?;
        std::fs::create_dir(&path)?;
        assert!(second.evidence().is_err());
        assert!(
            root.create_file(&format!("{}.mp4", uuid::Uuid::new_v4()))
                .is_err()
        );
        assert!(!path.join(second_key).exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn create_rejects_symlink_permissions_and_replaced_parent() -> anyhow::Result<()> {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let (path, root) = fixture()?;
        let key = format!("{}.mp4", uuid::Uuid::new_v4());
        let target = path.join("target");
        std::fs::write(&target, b"untouched")?;
        symlink(&target, path.join(&key))?;
        assert!(root.create_file(&key).is_err());
        assert_eq!(std::fs::read(&target)?, b"untouched");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777))?;
        assert!(
            root.create_file(&format!("{}.mp4", uuid::Uuid::new_v4()))
                .is_err()
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        let child = path.join("child");
        std::fs::create_dir(&child)?;
        let child_root = Root::open(&child)?;
        let mut owned = child_root.create_file(&format!("{}.mp4", uuid::Uuid::new_v4()))?;
        std::fs::rename(&path, path.with_extension("retained"))?;
        std::fs::create_dir_all(&child)?;
        assert!(owned.evidence().is_err());
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn owned_leaf_blocks_external_writes_and_replacement() -> anyhow::Result<()> {
        let (path, root) = fixture()?;
        let key = format!("{}.mp4", uuid::Uuid::new_v4());
        let mut owned = root.create_file(&key)?;
        owned.file_mut().write_all(b"retained")?;
        assert!(std::fs::write(path.join(&key), b"replacement").is_err());
        assert!(std::fs::rename(path.join(&key), path.join("other")).is_err());
        assert_eq!(owned.evidence()?.0, 8);
        Ok(())
    }
}
