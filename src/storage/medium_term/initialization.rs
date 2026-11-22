//! Keeps the exclusively created legacy file pinned until its MP4 header and catalog entry exist.
use super::WriterFile;
use cap_std::fs::{Dir, File, OpenOptions};
use std::{ffi::OsStr, io, path::Path};

pub(super) fn legacy<T>(
    path: &Path,
    initialize: impl FnOnce(WriterFile) -> io::Result<T>,
) -> io::Result<T> {
    let parent = Dir::open_ambient_dir(
        path.parent().ok_or(io::ErrorKind::InvalidInput)?,
        cap_std::ambient_authority(),
    )?;
    let name = path.file_name().ok_or(io::ErrorKind::InvalidInput)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    let owned = parent.open_with(name, &options)?;
    let result = owned
        .try_clone()
        .and_then(|file| initialize(WriterFile::Legacy(file.into_std())));
    if let Err(initialization) = &result
        && let Err(cleanup) = discard(&parent, name, owned)
    {
        return Err(io::Error::new(
            initialization.kind(),
            format!(
                "recording initialization failed ({initialization}); partial-file cleanup failed ({cleanup})"
            ),
        ));
    }
    result
}

#[cfg(windows)]
fn discard(_parent: &Dir, _name: &OsStr, file: File) -> io::Result<()> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{
            DELETE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, ReOpenFile,
        },
    };
    // Reopen the exclusive creation handle, even if a different file has since taken its name.
    // SAFETY: The source handle remains live. The returned handle is transferred exactly once.
    let selected = unsafe {
        ReOpenFile(
            HANDLE(file.as_raw_handle()),
            DELETE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_FLAG_OPEN_REPARSE_POINT,
        )
    }?;
    let removal = unsafe { std::fs::File::from_raw_handle(selected.0) };
    crate::storage::long_term::inspection::removal::windows::remove(File::from_std(removal))
}

#[cfg(unix)]
fn discard(parent: &Dir, name: &OsStr, file: File) -> io::Result<()> {
    use crate::storage::long_term::inspection::removal::{
        create_private_directory, unix::rename_to,
    };
    use cap_std::fs::MetadataExt;
    let owned = file.metadata()?;
    let matches = |entry: &cap_std::fs::Metadata| {
        entry.is_file() && entry.dev() == owned.dev() && entry.ino() == owned.ino()
    };
    match parent.symlink_metadata(name) {
        Ok(entry) if matches(&entry) => {}
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }

    // POSIX unlink operates on a name. Isolate that entry and compare it to the pinned file
    // again before unlinking. No operation can overwrite a replacement at the original path.
    let stage_name = format!(".keeppeek-header-{}", uuid::Uuid::new_v4());
    let stage_name = OsStr::new(&stage_name);
    let stage = create_private_directory(parent, stage_name)?;
    let leaf = OsStr::new("partial");
    let cleanup = (|| {
        rename_to(parent, name, &stage, leaf)?;
        if !matches(&stage.symlink_metadata(leaf)?) {
            return rename_to(&stage, leaf, parent, name);
        }
        stage.remove_file(leaf)
    })();
    if stage.entries()?.next().is_none() {
        parent.remove_dir(stage_name)?;
    }
    cleanup
}

#[cfg(not(any(unix, windows)))]
fn discard(_parent: &Dir, _name: &OsStr, _file: File) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Write};

    #[test]
    fn partial_header_failure_removes_only_the_exclusively_created_file() -> io::Result<()> {
        let root = std::env::temp_dir().join(format!("keeppeek-header-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root)?;
        let active = root.join("recording.mp4.active");
        let unrelated = root.join("unrelated.mp4.active");
        fs::write(&unrelated, b"keep")?;
        let result: io::Result<()> = legacy(&active, |mut file| {
            file.write_all(b"partial MP4 initialization")?;
            Err(io::Error::other("injected write failure"))
        });
        assert_eq!(result.unwrap_err().to_string(), "injected write failure");
        assert!(!active.exists());
        assert_eq!(fs::read(&unrelated)?, b"keep");
        assert_eq!(fs::read_dir(&root)?.count(), 1);
        fs::remove_dir_all(root)
    }

    #[test]
    fn partial_header_failure_preserves_a_replacement_at_the_active_name() -> io::Result<()> {
        let root = std::env::temp_dir().join(format!("keeppeek-header-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root)?;
        let active = root.join("recording.mp4.active");
        let result: io::Result<()> = legacy(&active, |mut file| {
            file.write_all(b"partial")?;
            fs::rename(&active, root.join("detached"))?;
            fs::write(&active, b"replacement")?;
            Err(io::Error::other("injected write failure"))
        });
        assert_eq!(result.unwrap_err().to_string(), "injected write failure");
        assert_eq!(fs::read(&active)?, b"replacement");
        fs::remove_dir_all(root)
    }
}
