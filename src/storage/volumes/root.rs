//! Pins a named root without following links or creating missing directories.

use cap_fs_ext::DirExt;
use cap_std::fs::Dir;
use std::{
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

mod file;
mod retirement;
pub use file::OwnedFile;

/// An opened directory and the identity that a binding must retain.
pub struct Root {
    path: PathBuf,
    directory: Dir,
    identity: Identity,
}

/// Filesystem and directory identities obtained from an open handle.
#[derive(Clone, PartialEq, Eq)]
pub struct Identity {
    pub filesystem: String,
    pub directory: String,
}

impl std::fmt::Debug for Root {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Root([REDACTED])")
    }
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Identity([REDACTED])")
    }
}

impl Root {
    pub(crate) fn matches_directory(&self, directory: &Dir) -> anyhow::Result<()> {
        self.revalidate()?;
        anyhow::ensure!(
            identity(directory)? == self.identity,
            "metadata directory is outside its bound root"
        );
        Ok(())
    }
    /// Opens each directory component without following symbolic links.
    /// The two-second deadline is cooperative; it cannot interrupt filesystem calls.
    ///
    /// # Errors
    /// Rejects invalid, missing, linked, inaccessible, or unsupported roots.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        super::validation::comparison_root(path)?;
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut anchor = PathBuf::new();
        let mut components = path.components().peekable();
        while let Some(component @ (Component::Prefix(_) | Component::RootDir)) = components.peek()
        {
            anchor.push(component.as_os_str());
            components.next();
        }
        let mut directory = Dir::open_ambient_dir(anchor, cap_std::ambient_authority())?;
        #[cfg(windows)]
        reject_reparse(&directory)?;
        for component in components {
            anyhow::ensure!(
                Instant::now() < deadline,
                "volume root inspection timed out"
            );
            anyhow::ensure!(
                matches!(component, Component::Normal(_)),
                "invalid root component"
            );
            directory = directory.open_dir_nofollow(component.as_os_str())?;
            #[cfg(windows)]
            reject_reparse(&directory)?;
        }
        let identity = identity(&directory)?;
        anyhow::ensure!(
            Instant::now() < deadline,
            "volume root inspection timed out"
        );
        Ok(Self {
            path: path.to_path_buf(),
            directory,
            identity,
        })
    }

    pub const fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Observes capacity on the pinned filesystem after reading the ledger revision.
    ///
    /// # Errors
    /// Rejects unavailable, replaced, unsupported, or slow roots. It never probes a parent.
    pub fn capacity(
        &self,
        ledger_revision: u64,
    ) -> anyhow::Result<crate::storage::catalog::locations::Capacity> {
        let observed_at = Instant::now();
        self.revalidate()?;
        let available_bytes = available_bytes(&self.directory)?;
        self.revalidate()?;
        anyhow::ensure!(
            observed_at.elapsed() < Duration::from_secs(2),
            "volume capacity inspection timed out"
        );
        Ok(crate::storage::catalog::locations::Capacity {
            ledger_revision,
            observed_at,
            available_bytes,
            filesystem: self.identity.filesystem.clone(),
            root_identity: self.identity.directory.clone(),
        })
    }

    /// Checks that the configured name still identifies the pinned directory.
    ///
    /// This observation does not authorize a later pathname-based write.
    ///
    /// # Errors
    /// Rejects missing roots, replaced roots, links, and changed identities.
    pub fn revalidate(&self) -> anyhow::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let current = Self::open(&self.path)?;
        anyhow::ensure!(
            current.identity == self.identity && identity(&self.directory)? == self.identity,
            "volume root identity changed"
        );
        anyhow::ensure!(
            Instant::now() < deadline,
            "volume root inspection timed out"
        );
        Ok(())
    }
}

#[cfg(windows)]
fn reject_reparse(directory: &Dir) -> anyhow::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FileAttributeTagInfo,
            GetFileInformationByHandleEx,
        },
    };
    let mut attributes = FILE_ATTRIBUTE_TAG_INFO::default();
    // SAFETY: The handle remains open and the output has the requested type and size.
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(directory.as_raw_handle()),
            FileAttributeTagInfo,
            std::ptr::from_mut(&mut attributes).cast(),
            u32::try_from(size_of::<FILE_ATTRIBUTE_TAG_INFO>())?,
        )?;
    }
    anyhow::ensure!(
        attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0,
        "volume roots cannot traverse reparse points"
    );
    Ok(())
}

#[cfg(windows)]
fn identity(directory: &Dir) -> anyhow::Result<Identity> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx},
    };
    let mut info = FILE_ID_INFO::default();
    // SAFETY: The handle remains open and the output has the requested type and size.
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(directory.as_raw_handle()),
            FileIdInfo,
            std::ptr::from_mut(&mut info).cast(),
            u32::try_from(size_of::<FILE_ID_INFO>())?,
        )?;
    }
    Ok(Identity {
        filesystem: format!("{}:{}", volume_path(directory)?, info.VolumeSerialNumber),
        directory: format!("{:032x}", u128::from_be_bytes(info.FileId.Identifier)),
    })
}

#[cfg(windows)]
fn volume_path(directory: &Dir) -> anyhow::Result<String> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{GetFinalPathNameByHandleW, VOLUME_NAME_GUID},
    };
    let mut buffer = [0_u16; 4_096];
    // SAFETY: The handle remains open and the API receives the buffer's actual length.
    let length = unsafe {
        GetFinalPathNameByHandleW(
            HANDLE(directory.as_raw_handle()),
            &mut buffer,
            VOLUME_NAME_GUID,
        )
    };
    anyhow::ensure!(
        length > 0 && (length as usize) < buffer.len(),
        "volume GUID path is unavailable"
    );
    let path = String::from_utf16(&buffer[..length as usize])?;
    let end = path
        .find("}\\")
        .ok_or_else(|| anyhow::anyhow!("invalid volume GUID path"))?
        + 2;
    anyhow::ensure!(
        path.starts_with(r"\\?\Volume{"),
        "root is not a local volume"
    );
    Ok(path[..end].to_ascii_lowercase())
}

#[cfg(windows)]
fn available_bytes(directory: &Dir) -> anyhow::Result<u64> {
    use windows::{Win32::Storage::FileSystem::GetDiskFreeSpaceExW, core::PCWSTR};
    // A volume GUID avoids querying a replacement drive-letter target.
    let path: Vec<u16> = volume_path(directory)?
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut available = 0;
    // SAFETY: The path is terminated and the output remains valid throughout the call.
    unsafe {
        GetDiskFreeSpaceExW(PCWSTR(path.as_ptr()), Some(&mut available), None, None)?;
    }
    Ok(available)
}

#[cfg(unix)]
fn available_bytes(directory: &Dir) -> anyhow::Result<u64> {
    let stats = rustix::fs::fstatvfs(directory)?;
    stats
        .f_bavail
        .checked_mul(stats.f_frsize)
        .ok_or_else(|| anyhow::anyhow!("volume capacity overflow"))
}

#[cfg(not(any(unix, windows)))]
fn available_bytes(_directory: &Dir) -> anyhow::Result<u64> {
    anyhow::bail!("volume capacity is unsupported on this platform")
}

#[cfg(unix)]
fn identity(directory: &Dir) -> anyhow::Result<Identity> {
    use cap_fs_ext::MetadataExt;
    let metadata = directory.dir_metadata()?;
    Ok(Identity {
        filesystem: metadata.dev().to_string(),
        directory: metadata.ino().to_string(),
    })
}

#[cfg(not(any(unix, windows)))]
fn identity(_directory: &Dir) -> anyhow::Result<Identity> {
    anyhow::bail!("volume root identity is unsupported on this platform")
}

#[cfg(test)]
mod tests {
    use super::Root;

    fn test_dir(name: &str) -> std::path::PathBuf {
        let base = std::env::temp_dir();
        #[cfg(unix)]
        let base = std::fs::canonicalize(base).unwrap();
        let path = base.join(format!("keeppeek-{name}-{:032x}", rand::random::<u128>()));
        std::fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn named_root_missing_does_not_create_or_use_parent() -> anyhow::Result<()> {
        let temporary = test_dir("named-root-missing");
        let missing = temporary.join("missing");
        assert!(Root::open(&missing).is_err());
        assert!(!missing.exists());
        Ok(())
    }

    #[test]
    fn named_root_identity_is_stable_and_distinguishes_directories() -> anyhow::Result<()> {
        let temporary = test_dir("named-root-identity");
        let first = temporary.join("first");
        let second = temporary.join("second");
        std::fs::create_dir(&first)?;
        std::fs::create_dir(&second)?;
        let root = Root::open(&first)?;
        root.revalidate()?;
        let other = Root::open(&second)?;
        assert!(root.identity().filesystem == other.identity().filesystem);
        assert!(root.identity().directory != other.identity().directory);
        let capacity = root.capacity(42)?;
        assert_eq!(capacity.ledger_revision, 42);
        assert_eq!(capacity.filesystem, root.identity().filesystem);
        assert_eq!(capacity.root_identity, root.identity().directory);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn named_root_rejects_missing_and_replaced_pinned_directory() -> anyhow::Result<()> {
        let temporary = test_dir("named-root-replaced");
        let original = temporary.join("original");
        std::fs::create_dir(&original)?;
        let root = Root::open(&original)?;
        std::fs::rename(&original, temporary.join("retained"))?;
        assert!(root.revalidate().is_err());
        assert!(root.capacity(1).is_err());
        std::fs::create_dir(&original)?;
        assert!(root.revalidate().is_err());
        assert!(root.capacity(1).is_err());
        assert!(Root::open(&original).is_ok());
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn named_root_pin_prevents_directory_replacement() -> anyhow::Result<()> {
        let temporary = test_dir("named-root-pinned");
        let original = temporary.join("original");
        std::fs::create_dir(&original)?;
        let root = Root::open(&original)?;
        let error = std::fs::rename(&original, temporary.join("retained")).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(32));
        root.revalidate()?;
        drop(root);
        std::fs::rename(&original, temporary.join("retained"))?;
        assert!(Root::open(&original).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn named_root_rejects_final_and_intermediate_symlinks() -> anyhow::Result<()> {
        let temporary = test_dir("named-root-symlink");
        let original = temporary.join("original");
        std::fs::create_dir_all(original.join("child"))?;
        let link = temporary.join("link");
        std::os::unix::fs::symlink(&original, &link)?;
        assert!(Root::open(&link).is_err());
        assert!(Root::open(&link.join("child")).is_err());
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn named_root_rejects_final_and_intermediate_junctions() -> anyhow::Result<()> {
        let temporary = test_dir("named-root-junction");
        std::fs::create_dir_all(temporary.join("original/child"))?;
        let output = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command",
                "$ErrorActionPreference = 'Stop'; New-Item -ItemType Junction -Path (Join-Path $env:KEEPPEEK_ROOT_TEST 'link') -Target (Join-Path $env:KEEPPEEK_ROOT_TEST 'original') | Out-Null"])
            .env("KEEPPEEK_ROOT_TEST", &temporary)
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "junction fixture failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(Root::open(&temporary.join("link")).is_err());
        assert!(Root::open(&temporary.join("link/child")).is_err());
        Ok(())
    }
}
