use std::{
    fs::{self, File},
    io::{self, Read},
    path::Path,
};
pub(super) fn staged_name(id: uuid::Uuid) -> String {
    format!(".keeppeek-{id}.partial")
}

pub(super) fn remove_stage(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => fs::remove_file(path),
        Ok(_) => Err(io::Error::other("recording stage is not a regular file")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Two spellings of the same parent may name the only directory entry.
pub(super) fn same_entry(source: &Path, destination: &Path) -> io::Result<bool> {
    if source.file_name() != destination.file_name() {
        return Ok(false);
    }
    let (Some(first), Some(second)) = (source.parent(), destination.parent()) else {
        return Err(io::Error::other("missing recording parent"));
    };
    match second.canonicalize() {
        Ok(second) => Ok(first.canonicalize()? == second),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Keep the published source readable until its catalog pointer has switched.
pub(super) fn stage_source(source: &Path, destination: &Path, stage: uuid::Uuid) -> io::Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::other("missing recording directory"))?;
    fs::create_dir_all(parent)?;
    remove_stage(&parent.join(staged_name(stage)))?;
    match fs::hard_link(source, destination) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            return verify_existing(source, destination);
        }
        Err(_) => {}
    }
    copy_and_publish(source, destination, parent, stage)
}

fn copy_and_publish(
    source: &Path,
    destination: &Path,
    parent: &Path,
    stage: uuid::Uuid,
) -> io::Result<()> {
    let directory = cap_std::fs::Dir::open_ambient_dir(parent, cap_std::ambient_authority())?;
    let temporary = staged_name(stage);
    let mut options = cap_std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt;
        use windows::Win32::{Foundation::GENERIC_WRITE, Storage::FileSystem::DELETE};
        options.access_mode(GENERIC_WRITE.0 | DELETE.0);
    }
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        use std::os::unix::fs::PermissionsExt;
        options.mode(fs::metadata(source)?.permissions().mode() & 0o600);
    }
    let mut staged = directory.open_with(&temporary, &options)?;
    let copied = (|| {
        io::copy(&mut File::open(source)?, &mut staged)?;
        staged.sync_all()?;
        let name = destination
            .file_name()
            .ok_or_else(|| io::Error::other("missing recording name"))?;
        use crate::storage::long_term::inspection::removal;
        #[cfg(windows)]
        {
            removal::windows::rename_to(&staged, &directory, name)
        }
        #[cfg(unix)]
        {
            publish_no_replace(&directory, temporary.as_ref(), name, || {
                removal::unix::rename_to(&directory, temporary.as_ref(), &directory, name)
            })
        }
        #[cfg(not(any(windows, unix)))]
        {
            Err(io::Error::other(
                "recording publication is unsupported on this platform",
            ))
        }
    })();
    drop(staged);
    if copied.is_err() {
        let _ = directory.remove_file(&temporary);
    }
    // Windows can report a failed no-replace handle rename as a generic I/O
    // error. The destination itself decides whether a previous publication can
    // be reused; its complete contents must agree with the retained source.
    match copied {
        Err(_) if destination.try_exists()? => verify_existing(source, destination),
        other => other,
    }
}

#[cfg(any(unix, test))]
fn publish_no_replace(
    directory: &cap_std::fs::Dir,
    temporary: &std::ffi::OsStr,
    name: &std::ffi::OsStr,
    rename: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    match rename() {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Err(error),
        Err(_) => {
            directory.hard_link(temporary, directory, name)?;
            directory.remove_file(temporary)
        }
    }
}

pub(super) fn verify_existing(source: &Path, destination: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(destination)?;
    let mut left = File::open(source)?;
    if !metadata.is_file() || metadata.len() != left.metadata()?.len() {
        return Err(io::Error::other(
            "conflicting finalized recording destination",
        ));
    }
    let mut right = File::open(destination)?;
    let mut left_buf = [0; 8192];
    let mut right_buf = [0; 8192];
    loop {
        let count = left.read(&mut left_buf)?;
        if count == 0 {
            return Ok(());
        }
        right.read_exact(&mut right_buf[..count])?;
        if left_buf[..count] != right_buf[..count] {
            return Err(io::Error::other("conflicting finalized recording contents"));
        }
    }
}

#[cfg(test)]
mod tests;
