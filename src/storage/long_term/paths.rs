use std::path::{Component, Path};

pub(super) fn relative_components<'a>(root: &Path, path: &'a Path) -> Option<&'a Path> {
    let relative = match path.strip_prefix(root) {
        Ok(relative) => relative,
        #[cfg(windows)]
        Err(_) => {
            let mut candidate = path.components();
            for expected in root.components() {
                if !candidate
                    .next()?
                    .as_os_str()
                    .eq_ignore_ascii_case(expected.as_os_str())
                {
                    return None;
                }
            }
            candidate.as_path()
        }
        #[cfg(not(windows))]
        Err(_) => return None,
    };
    relative
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
        .then_some(relative)
}

pub(super) fn recording_relative<'a>(root: &Path, path: &'a Path) -> Option<&'a Path> {
    let relative = relative_components(root, path)?;
    #[cfg(windows)]
    if !path.starts_with(root) {
        // Windows supports case-sensitive directories. An alternate spelling must
        // name the configured root itself before a catalog entry can be retired.
        let prefix = path.ancestors().nth(relative.components().count())?;
        if root.canonicalize().ok()? != prefix.canonicalize().ok()? {
            return None;
        }
    }
    Some(relative)
}

pub(super) fn contains_private_directory(path: &Path) -> bool {
    path.components().any(|part| {
        [".exports", super::layout::HEADER_RECOVERY_DIRECTORY]
            .iter()
            .any(|name| {
                if cfg!(windows) {
                    part.as_os_str().eq_ignore_ascii_case(name)
                } else {
                    part.as_os_str() == *name
                }
            })
    })
}

#[cfg(test)]
mod tests {
    use super::super::LongTermStore;
    use std::{fs, io};

    #[test]
    fn recording_cleanup_rejects_traversal_and_adjacent_root() {
        let root = std::env::temp_dir().join(format!("keeppeek-cleanup-{}", uuid::Uuid::new_v4()));
        let archive = root.join("archive");
        let capture = archive.join("incoming");
        let adjacent = root.join("archive-old");
        fs::create_dir_all(&capture).unwrap();
        fs::create_dir_all(&adjacent).unwrap();
        let file = adjacent.join("keep.mp4");
        fs::write(&file, b"footage").unwrap();
        let store = LongTermStore::for_recording_roots(&archive, &capture);
        for path in [&file, &archive.join("../archive-old/keep.mp4")] {
            assert!(!store.contains_cleanup_path(path, "front/main"));
            assert_eq!(
                store.remove_catalog_recording(path).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
        }
        assert_eq!(fs::read(file).unwrap(), b"footage");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn recording_cleanup_recognizes_existing_windows_root_case_and_preserves_capture_and_exports() {
        let root = std::env::temp_dir().join(format!("keeppeek-case-{}", uuid::Uuid::new_v4()));
        let archive = root.join("Archive");
        let capture = root.join("ARCHIVE/Capture");
        let published = archive.join("front/main/2026-11-22/17/0000001.mp4");
        let pending = archive.join("capture/front/main/2026-11-22/17/0000001.mp4");
        let export = archive.join(".exports/keep.mp4");
        for file in [&published, &pending, &export] {
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, b"media").unwrap();
        }
        let store = LongTermStore::for_recording_roots(&archive, &capture);
        let alternate = root.join("ARCHIVE/front/main/2026-11-22/17/0000001.mp4");
        let output = root.join("ARCHIVE/.EXPORTS/keep.mp4");
        let recovery = archive
            .join(super::super::layout::HEADER_RECOVERY_DIRECTORY.to_ascii_uppercase())
            .join("keep.mp4");
        fs::create_dir_all(recovery.parent().unwrap()).unwrap();
        fs::write(&recovery, b"held").unwrap();
        assert!(store.remove_catalog_recording(&recovery).is_err());
        assert!(recovery.is_file());
        assert!(store.contains_cleanup_path(&alternate, "front/main"));
        assert!(!store.contains_cleanup_path(&pending, "front/main"));
        assert!(!store.contains_cleanup_path(&output, "front/main"));
        assert_eq!(
            store.remove_catalog_recording(&output).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(store.remove_catalog_recording(&alternate).unwrap(), 5);
        assert!(archive.is_dir());
        assert!(pending.is_file() && export.is_file());
        fs::remove_dir_all(root).unwrap();
    }
}
