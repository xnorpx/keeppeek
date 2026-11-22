//! Keeps the exclusively created legacy file pinned until its MP4 header and catalog entry exist.
use super::WriterFile;
#[cfg(not(unix))]
use cap_std::fs::File;
use cap_std::fs::{Dir, OpenOptions};
#[cfg(not(unix))]
use std::ffi::OsStr;
use std::{io, path::Path};

#[cfg(unix)]
mod unix;

pub(super) fn legacy<T>(
    path: &Path,
    initialize: impl FnOnce(WriterFile) -> io::Result<T>,
) -> io::Result<T> {
    let parent = Dir::open_ambient_dir(
        path.parent().ok_or(io::ErrorKind::InvalidInput)?,
        cap_std::ambient_authority(),
    )?;
    let name = path.file_name().ok_or(io::ErrorKind::InvalidInput)?;
    #[cfg(unix)]
    unix::restore_pending(&parent, name)?;
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
use unix::discard;

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
        #[cfg(unix)]
        assert_eq!(
            fs::read_dir(root.join(unix::RECOVERY_DIRECTORY))?.count(),
            0
        );
        #[cfg(windows)]
        assert_eq!(fs::read_dir(&root)?.count(), 1);
        fs::remove_dir_all(root)
    }

    #[cfg(unix)]
    #[test]
    fn nonprivate_recovery_entries_are_untouched_when_a_new_recording_starts() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let root =
            std::env::temp_dir().join(format!("keeppeek-header-mode-{}", uuid::Uuid::new_v4()));
        let stage = root.join(unix::RECOVERY_DIRECTORY);
        fs::create_dir_all(&stage)?;
        fs::set_permissions(&stage, fs::Permissions::from_mode(0o755))?;
        let name = "recording.mp4.active";
        fs::write(stage.join(name), b"untrusted staged file")?;
        fs::write(stage.join(format!("{name}.origin")), b"untrusted marker")?;
        legacy(&root.join(name), |mut file| file.write_all(b"new header"))?;
        assert_eq!(fs::read(root.join(name))?, b"new header");
        assert_eq!(fs::read(stage.join(name))?, b"untrusted staged file");
        assert_eq!(
            fs::read(stage.join(format!("{name}.origin")))?,
            b"untrusted marker"
        );
        assert_eq!(fs::read_dir(&stage)?.count(), 2);
        assert_eq!(
            legacy(&root.join(name), |_| Ok(())).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
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
