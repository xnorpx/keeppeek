//! Inspects recording file metadata without authorizing or performing deletion.
//!
//! These metadata observations detect drift for maintenance previews. They do not
//! reserve files, verify container contents, or authorize deletion. Revalidation
//! followed by pathname-based deletion still has a replacement race.
//! Unchanged metadata does not prove unchanged bytes. Creation time is compared
//! when available. The 64-bit Windows file index is not a complete ReFS identity.
//! Each observation keeps its file open until dropped. This pins that object,
//! not its pathname, and does not prevent another handle from changing its bytes.

use cap_fs_ext::{DirExt, FollowSymlinks, MetadataExt, OpenOptionsFollowExt};
use cap_std::fs::{Dir, File, Metadata, OpenOptions};
use sha2::{Digest, Sha256};
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

pub(in crate::storage) const PATH_BYTES_MAX: usize = 4_096;
/// Two decimal u64 identifiers and their separator fit within this bound.
pub(in crate::storage) const IDENTITY_BYTES_MAX: usize = 41;
const PATH_COMPONENTS_MAX: usize = 16;
const INSPECTION_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::storage) struct Identity {
    device: u64,
    file: u64,
}

impl Identity {
    pub(in crate::storage) fn parse(value: &str) -> Option<Self> {
        if value.len() > IDENTITY_BYTES_MAX {
            return None;
        }
        let (device, file) = value.split_once(':')?;
        if !device.bytes().all(|byte| byte.is_ascii_digit())
            || !file.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        Some(Self {
            device: device.parse().ok()?,
            file: file.parse().ok()?,
        })
    }

    pub(in crate::storage) fn fingerprint(self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(self.device.to_be_bytes());
        digest.update(self.file.to_be_bytes());
        digest.finalize().into()
    }
}

impl fmt::Debug for Identity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Identity([REDACTED])")
    }
}

/// Holds a configured archive directory open for bounded, read-only inspection.
pub struct Archive {
    root: PathBuf,
    directory: Dir,
    instance: u128,
}

/// Pins one file and captures metadata for reinspection through the same archive instance.
pub struct Observation {
    relative: PathBuf,
    file: File,
    metadata: Metadata,
    instance: u128,
}

impl Archive {
    /// Opens a trusted configured root once, without enumerating its contents.
    ///
    /// The caller must control the root and its ancestors during this operation.
    /// Descendant paths are inspected relative to the resulting directory handle.
    ///
    /// # Errors
    /// Returns an error if the root cannot be opened as a directory.
    pub fn open(root: impl AsRef<Path>) -> std::io::Result<Self> {
        let root = root.as_ref().to_path_buf();
        let directory = Dir::open_ambient_dir(&root, cap_std::ambient_authority())?;
        Ok(Self {
            root,
            directory,
            instance: rand::random(),
        })
    }

    /// Observes one catalog-resolved recording without reading its media bytes.
    ///
    /// Supply a trusted catalog path and its observed byte count, never a client
    /// path. Work is limited to sixteen components and metadata operations. The
    /// two-second deadline is cooperative; it cannot preempt an operating-system
    /// call. The observation retains one read-only handle, released on drop.
    /// Unix requires read permission; Windows requests only file attributes.
    /// No file or catalog state is changed.
    ///
    /// # Errors
    /// Rejects escapes, hidden directories, links, non-files, size drift,
    /// overlong paths, expired work, and filesystem errors.
    pub fn inspect(
        &self,
        catalog_path: impl AsRef<Path>,
        catalog_bytes: u64,
    ) -> std::io::Result<Observation> {
        self.inspect_until(
            catalog_path,
            catalog_bytes,
            Instant::now() + INSPECTION_TIMEOUT,
        )
    }

    pub(in crate::storage) fn inspect_until(
        &self,
        catalog_path: impl AsRef<Path>,
        catalog_bytes: u64,
        deadline: Instant,
    ) -> std::io::Result<Observation> {
        let deadline = deadline.min(Instant::now() + INSPECTION_TIMEOUT);
        check_deadline(deadline)?;
        let relative = catalog_path
            .as_ref()
            .strip_prefix(&self.root)
            .map_err(|_| denied())?;
        validate_relative(relative)?;
        let (file, metadata) = self.open_file(relative, deadline)?;
        if metadata.len() != catalog_bytes {
            return Err(changed());
        }
        Ok(Observation {
            relative: relative.to_path_buf(),
            file,
            metadata,
            instance: self.instance,
        })
    }

    /// Checks for observed metadata drift without granting authority to delete.
    ///
    /// # Errors
    /// Rejects another archive instance, changed identity or metadata, links,
    /// missing files, and filesystem errors. Success is a point-in-time check,
    /// not a lock or a guarantee about later filesystem operations.
    pub fn revalidate(&self, observation: &Observation) -> std::io::Result<()> {
        self.revalidate_until(observation, Instant::now() + INSPECTION_TIMEOUT)
    }

    pub(in crate::storage) fn revalidate_until(
        &self,
        observation: &Observation,
        deadline: Instant,
    ) -> std::io::Result<()> {
        let deadline = deadline.min(Instant::now() + INSPECTION_TIMEOUT);
        check_deadline(deadline)?;
        if observation.instance != self.instance {
            return Err(changed());
        }
        let pinned = observation.file.metadata()?;
        eligible(&pinned)?;
        if !same_metadata(&pinned, &observation.metadata)? {
            return Err(changed());
        }
        let (_file, current) = self.open_file(&observation.relative, deadline)?;
        if !same_metadata(&current, &observation.metadata)? {
            return Err(changed());
        }
        check_deadline(deadline)?;
        Ok(())
    }

    fn open_file(&self, relative: &Path, deadline: Instant) -> std::io::Result<(File, Metadata)> {
        check_deadline(deadline)?;
        let mut directory = self.directory.try_clone()?;
        let mut components = relative.components().peekable();
        while let Some(component) = components.next() {
            check_deadline(deadline)?;
            if components.peek().is_some() {
                directory = directory.open_dir_nofollow(component.as_os_str())?;
            } else {
                eligible(&directory.symlink_metadata(component.as_os_str())?)?;
                return open_leaf(&directory, component.as_os_str(), deadline);
            }
        }
        Err(denied())
    }
}

fn open_leaf(
    directory: &Dir,
    name: &std::ffi::OsStr,
    deadline: Instant,
) -> std::io::Result<(File, Metadata)> {
    check_deadline(deadline)?;
    let file = directory.open_with(name, &observation_options())?;
    let metadata = file.metadata()?;
    eligible(&metadata)?;
    check_deadline(deadline)?;
    Ok((file, metadata))
}

fn observation_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(false)
        .create(false)
        .truncate(false)
        .follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_fs_ext::OpenOptionsSyncExt;
        options.nonblock(true);
    }
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::{
            FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        options
            .access_mode(FILE_READ_ATTRIBUTES.0)
            .share_mode((FILE_SHARE_DELETE | FILE_SHARE_READ | FILE_SHARE_WRITE).0);
    }
    options
}

fn eligible(metadata: &Metadata) -> std::io::Result<()> {
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.nlink() != 1 {
        return Err(denied());
    }
    Ok(())
}

fn same_metadata(current: &Metadata, previous: &Metadata) -> std::io::Result<bool> {
    Ok(current.dev() == previous.dev()
        && current.ino() == previous.ino()
        && current.len() == previous.len()
        && current.permissions() == previous.permissions()
        && current.modified()? == previous.modified()?
        && current.created().ok() == previous.created().ok())
}

impl Observation {
    /// Returns the observed logical file size, not a promise of reclaimable space.
    pub const fn file_bytes(&self) -> u64 {
        self.metadata.len()
    }

    pub(in crate::storage) fn identity(&self) -> Identity {
        Identity {
            device: self.metadata.dev(),
            file: self.metadata.ino(),
        }
    }
}

impl fmt::Debug for Archive {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Archive").finish_non_exhaustive()
    }
}

impl fmt::Debug for Observation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Observation")
            .field("file_bytes", &self.file_bytes())
            .finish_non_exhaustive()
    }
}

fn validate_relative(relative: &Path) -> std::io::Result<()> {
    if relative.as_os_str().len() > PATH_BYTES_MAX
        || relative
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("mp4")
    {
        return Err(denied());
    }
    let mut count = 0;
    for component in relative.components() {
        count += 1;
        let Component::Normal(name) = component else {
            return Err(denied());
        };
        let Some(name) = name.to_str() else {
            return Err(denied());
        };
        if count > PATH_COMPONENTS_MAX || name.starts_with('.') || name.contains(['\\', ':', '\0'])
        {
            return Err(denied());
        }
    }
    Ok(())
}

fn check_deadline(deadline: Instant) -> std::io::Result<()> {
    if Instant::now() >= deadline {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "recording inspection expired",
        ));
    }
    Ok(())
}

fn denied() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "recording path is not eligible for inspection",
    )
}

fn changed() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "recording observation changed",
    )
}

#[cfg(test)]
mod tests;
