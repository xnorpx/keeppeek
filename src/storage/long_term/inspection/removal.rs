use super::{Archive, Observation, check_deadline, eligible, open_leaf, same_metadata};
use crate::storage::catalog::maintenance::{FileIdentity, jobs::claims::Claim};
#[cfg(not(windows))]
use cap_fs_ext::OpenOptionsMaybeDirExt;
use cap_fs_ext::{DirExt, MetadataExt};
use cap_std::fs::Dir;
#[cfg(not(windows))]
use cap_std::fs::{DirBuilder, OpenOptions};
use std::{
    ffi::OsStr,
    io,
    path::Component,
    time::{Duration, Instant},
};

const REMOVAL_BUDGET: Duration = Duration::from_secs(2);
const STAGED_FILE: &str = "recording.mp4";

#[cfg(windows)]
mod windows;

#[cfg(unix)]
mod unix;

#[cfg(any(windows, test))]
mod ace;

pub(in crate::storage) struct Staged {
    directory: Dir,
    directory_identity: FileIdentity,
    observation: Observation,
    deadline: Instant,
    #[cfg(windows)]
    removal_handle: cap_std::fs::File,
}

impl Archive {
    pub(in crate::storage) fn validate_removal(&self) -> io::Result<()> {
        validate_owner(&self.directory, 0o022)?;
        sync_directory(&self.directory)
    }

    pub(in crate::storage) fn validate_unstaged_claim(&self, claim: &Claim) -> io::Result<()> {
        let observation = self.inspect(&claim.path, claim.file_bytes)?;
        validate_claim(&observation, claim)
    }

    pub(in crate::storage) fn has_staged_claim(&self, claim: &Claim) -> io::Result<bool> {
        let result = private_directory(&self.directory, OsStr::new(".maintenance"), false)
            .and_then(|staging| private_directory(&staging, OsStr::new(&claim.token), false))
            .and_then(|directory| staging_contents(&directory));
        match result {
            Ok(present) => Ok(present),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub(in crate::storage) fn check_removed_claim(
        &self,
        claim: &Claim,
        checkpoint: FileIdentity,
        deadline: Instant,
    ) -> io::Result<()> {
        let deadline = deadline.min(Instant::now() + REMOVAL_BUDGET);
        check_deadline(deadline)?;
        self.validate_removal()?;
        check_deadline(deadline)?;
        let staging = private_directory(&self.directory, OsStr::new(".maintenance"), false)?;
        check_deadline(deadline)?;
        let directory = private_directory(&staging, OsStr::new(&claim.token), false)?;
        check_deadline(deadline)?;
        if directory_identity(&directory)? != checkpoint || staging_contents(&directory)? {
            return Err(super::changed());
        }
        let relative = claim
            .path
            .strip_prefix(&self.root)
            .map_err(|_| super::denied())?;
        super::validate_relative(relative)?;
        let (parent, name) = self.removal_parent(relative, deadline)?;
        if let Some(name) = name {
            match parent.symlink_metadata(name) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                _ => return Err(super::changed()),
            }
        }
        check_deadline(deadline)?;
        sync_directory(&parent)?;
        check_deadline(deadline)?;
        sync_directory(&directory)?;
        check_deadline(deadline)
    }

    pub(in crate::storage) fn stage_claim(
        &self,
        claim: &Claim,
        checkpoint: Option<FileIdentity>,
    ) -> io::Result<Option<Staged>> {
        let deadline = Instant::now() + REMOVAL_BUDGET;
        self.validate_removal()?;
        let staging = private_directory(
            &self.directory,
            OsStr::new(".maintenance"),
            checkpoint.is_none(),
        )?;
        let directory =
            private_directory(&staging, OsStr::new(&claim.token), checkpoint.is_none())?;
        let directory_identity = directory_identity(&directory)?;
        if checkpoint.is_some_and(|expected| expected != directory_identity) {
            return Err(super::changed());
        }
        check_deadline(deadline)?;
        let relative = claim
            .path
            .strip_prefix(&self.root)
            .map_err(|_| super::denied())?;
        super::validate_relative(relative)?;
        let (parent, name) = self.removal_parent(relative, deadline)?;
        sync_directory(&parent)?;
        sync_directory(&directory)?;
        match self.open_staged(&directory, claim, deadline) {
            Ok(staged) => return Ok(Some(staged)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if staging_contents(&directory)? {
            return Err(super::changed());
        }
        if checkpoint.is_some() {
            if let Some(name) = name {
                match parent.symlink_metadata(name) {
                    Ok(_) => return Err(super::changed()),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            sync_directory(&parent)?;
            sync_directory(&directory)?;
            check_deadline(deadline)?;
            return Ok(None);
        }
        let name = name.ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        self.move_claim(claim, &parent, &name, directory, deadline)
            .map(Some)
    }

    fn open_staged(&self, directory: &Dir, claim: &Claim, deadline: Instant) -> io::Result<Staged> {
        if !staging_contents(directory)? {
            return Err(io::ErrorKind::NotFound.into());
        }
        #[cfg(windows)]
        let removal_handle = windows::exclusive_file(directory, OsStr::new(STAGED_FILE))?;
        let (file, metadata) = open_leaf(directory, OsStr::new(STAGED_FILE), deadline)?;
        let observation = Observation {
            relative: STAGED_FILE.into(),
            file,
            metadata,
            instance: self.instance,
        };
        validate_claim(&observation, claim)?;
        Ok(Staged {
            directory: directory.try_clone()?,
            directory_identity: directory_identity(directory)?,
            observation,
            deadline,
            #[cfg(windows)]
            removal_handle,
        })
    }

    fn move_claim(
        &self,
        claim: &Claim,
        parent: &Dir,
        name: &OsStr,
        directory: Dir,
        deadline: Instant,
    ) -> io::Result<Staged> {
        let observation = self.inspect_until(&claim.path, claim.file_bytes, deadline)?;
        validate_claim(&observation, claim)?;
        check_deadline(deadline)?;
        #[cfg(windows)]
        let removal_handle = windows::exclusive_file(parent, name)?;
        #[cfg(windows)]
        {
            if !same_metadata(&removal_handle.metadata()?, &observation.metadata)? {
                return Err(super::changed());
            }
            windows::rename(&removal_handle, &directory)?;
        }
        #[cfg(unix)]
        unix::rename(parent, name, &directory)?;
        #[cfg(not(any(unix, windows)))]
        return Err(io::ErrorKind::Unsupported.into());
        sync_directory(parent)?;
        sync_directory(&directory)?;
        let (file, metadata) = open_leaf(&directory, OsStr::new(STAGED_FILE), deadline)?;
        if !same_metadata(&metadata, &observation.metadata)? {
            return Err(super::changed());
        }
        let observation = Observation {
            relative: STAGED_FILE.into(),
            file,
            metadata,
            instance: self.instance,
        };
        validate_claim(&observation, claim)?;
        Ok(Staged {
            directory_identity: directory_identity(&directory)?,
            directory,
            observation,
            deadline,
            #[cfg(windows)]
            removal_handle,
        })
    }

    fn removal_parent(
        &self,
        relative: &std::path::Path,
        deadline: Instant,
    ) -> io::Result<(Dir, Option<std::ffi::OsString>)> {
        let mut directory = self.directory.try_clone()?;
        let mut components = relative.components().peekable();
        while let Some(Component::Normal(name)) = components.next() {
            check_deadline(deadline)?;
            if components.peek().is_none() {
                return Ok((directory, Some(name.to_owned())));
            }
            directory = match directory.open_dir_nofollow(name) {
                Ok(child) => child,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    return Ok((directory, None));
                }
                Err(error) => return Err(error),
            };
            validate_owner(&directory, 0o022)?;
        }
        Err(super::denied())
    }
}

impl Staged {
    pub(in crate::storage) const fn directory_identity(&self) -> FileIdentity {
        self.directory_identity
    }

    pub(in crate::storage) fn remove(self) -> io::Result<()> {
        self.remove_with(|| Ok(()))
    }

    fn remove_with(self, before_unlink: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
        check_deadline(self.deadline)?;
        if !staging_contents(&self.directory)? {
            return Err(super::changed());
        }
        let metadata = self.observation.file.metadata()?;
        eligible(&metadata)?;
        if !same_metadata(&metadata, &self.observation.metadata)? {
            return Err(super::changed());
        }
        let (_file, current) = open_leaf(&self.directory, OsStr::new(STAGED_FILE), self.deadline)?;
        if !same_metadata(&current, &metadata)? {
            return Err(super::changed());
        }
        before_unlink()?;
        #[cfg(windows)]
        windows::remove(self.removal_handle)?;
        #[cfg(not(windows))]
        self.directory.remove_file(STAGED_FILE)?;
        sync_directory(&self.directory)?;
        #[cfg(unix)]
        if self.observation.file.metadata()?.nlink() != 0 {
            return Err(super::changed());
        }
        if staging_contents(&self.directory)? {
            return Err(super::changed());
        }
        Ok(())
    }
}

fn staging_contents(directory: &Dir) -> io::Result<bool> {
    let mut entries = directory.entries()?;
    let Some(entry) = entries.next().transpose()? else {
        return Ok(false);
    };
    if entry.file_name() != STAGED_FILE || entries.next().transpose()?.is_some() {
        return Err(super::changed());
    }
    Ok(true)
}

fn directory_identity(directory: &Dir) -> io::Result<FileIdentity> {
    let metadata = directory.dir_metadata()?;
    Ok(FileIdentity::from_observed(super::Identity {
        device: metadata.dev(),
        file: metadata.ino(),
    }))
}

fn validate_claim(observation: &Observation, claim: &Claim) -> io::Result<()> {
    if FileIdentity::from_observed(observation.identity()) != claim.file_identity
        || observation.file_bytes() != claim.file_bytes
    {
        return Err(super::changed());
    }
    Ok(())
}

fn private_directory(parent: &Dir, name: &OsStr, create: bool) -> io::Result<Dir> {
    #[cfg(not(windows))]
    let mut builder = DirBuilder::new();
    #[cfg(not(windows))]
    builder.recursive(false);
    #[cfg(unix)]
    {
        use cap_std::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    if create {
        #[cfg(windows)]
        let created = windows::create_private(parent, name);
        #[cfg(not(windows))]
        let created = parent.create_dir_with(name, &builder);
        match created {
            Ok(()) => sync_directory(parent)?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    let directory = parent.open_dir_nofollow(name)?;
    validate_owner(&directory, 0o077)?;
    sync_directory(parent)?;
    sync_directory(&directory)?;
    Ok(directory)
}

fn validate_owner(directory: &Dir, forbidden: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use cap_std::fs::{MetadataExt, PermissionsExt};
        let metadata = directory.dir_metadata()?;
        if metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.permissions().mode() & forbidden != 0
        {
            return Err(super::denied());
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        windows::validate_directory(directory, forbidden == 0o077)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (directory, forbidden);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "private staging permissions are not qualified on this platform",
        ))
    }
}

fn sync_directory(directory: &Dir) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows::sync(directory)
    }
    #[cfg(not(windows))]
    {
        let mut options = OpenOptions::new();
        options.read(true).write(false).maybe_dir(true);
        directory.open_with(".", &options)?.sync_all()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn substituted_unlink_does_not_report_the_selected_file_as_removed() {
        let root =
            std::env::temp_dir().join(format!("keeppeek-unlink-{:032x}", rand::random::<u128>()));
        let staging = root.join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        let file = staging.join(STAGED_FILE);
        std::fs::write(&file, [42; 8]).unwrap();
        let archive = Archive::open(&staging).unwrap();
        let staged = Staged {
            directory: archive.directory.try_clone().unwrap(),
            directory_identity: directory_identity(&archive.directory).unwrap(),
            observation: archive.inspect(&file, 8).unwrap(),
            deadline: Instant::now() + REMOVAL_BUDGET,
        };
        let retained = root.join("retained.mp4");
        let result = staged.remove_with(|| {
            std::fs::rename(&file, &retained)?;
            std::fs::write(&file, [24; 8])
        });
        let remaining = std::fs::read(&retained).unwrap();
        drop(archive);
        std::fs::remove_dir_all(root).unwrap();
        assert!(result.is_err());
        assert_eq!(remaining, [42; 8]);
    }
}
