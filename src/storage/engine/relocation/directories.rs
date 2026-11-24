use std::{fs, io, path::Path};

fn sync(directory: &Path) -> io::Result<()> {
    let directory = cap_std::fs::Dir::open_ambient_dir(directory, cap_std::ambient_authority())?;
    crate::storage::long_term::inspection::removal::sync_directory(&directory)
}

pub(super) fn sync_parent(path: &Path) -> io::Result<()> {
    sync(
        path.parent()
            .ok_or_else(|| io::Error::other("missing recording directory"))?,
    )
}

pub(super) fn ensure_root(root: &Path) -> io::Result<()> {
    if root.as_os_str().is_empty() || root.is_dir() {
        return Ok(());
    }
    let parent = root
        .parent()
        .ok_or_else(|| io::Error::other("missing recording volume"))?;
    ensure_root(parent)?;
    match fs::create_dir(root) {
        Ok(()) => {
            if let Err(error) = sync(if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            }) {
                // An empty entry whose parent could not be flushed is safe to retry.
                let _ = fs::remove_dir(root);
                return Err(error);
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && root.is_dir() => Ok(()),
        Err(error) => Err(error),
    }
}

pub(super) fn sync_recording(root: &Path, recording: &Path) -> io::Result<()> {
    sync_recording_with(root, recording, sync)
}

fn sync_recording_with(
    root: &Path,
    recording: &Path,
    mut flush: impl FnMut(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let relative = recording.strip_prefix(root).map_err(io::Error::other)?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(io::Error::other("invalid recording directory"));
    }
    // The leaf flush publishes the file name. Each parent flush publishes the
    // directory below it, up to the archive root owned by this handoff.
    for parent in recording
        .ancestors()
        .skip(1)
        .take(relative.components().count())
    {
        flush(if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recording_directory_sync_includes_new_calendar_ancestors_and_propagates_failure() {
        let root = std::env::temp_dir().join(format!("keeppeek-sync-{}", uuid::Uuid::new_v4()));
        let archive = root.join("new-volume/archive");
        let media = archive.join("cam/main/2026-11-22/17/0000001.mp4");
        ensure_root(&archive).unwrap();
        fs::create_dir_all(media.parent().unwrap()).unwrap();
        fs::write(&media, b"recording").unwrap();
        sync_recording(&archive, &media).unwrap();
        let mut seen = Vec::new();
        sync_recording_with(&archive, &media, |directory| {
            seen.push(directory.to_owned());
            Ok(())
        })
        .unwrap();
        assert_eq!(
            seen,
            [
                "cam/main/2026-11-22/17",
                "cam/main/2026-11-22",
                "cam/main",
                "cam",
                ""
            ]
            .map(|dir| archive.join(dir))
        );
        let mut calls = 0;
        let err = sync_recording_with(&archive, &media, |directory| {
            calls += 1;
            if directory == archive.join("cam/main") {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Ok(())
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(calls, 3);
        fs::remove_dir_all(root).unwrap();
    }
}
