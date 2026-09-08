use cap_std::fs::Dir;
use std::{ffi::OsStr, io};

pub(super) fn rename(parent: &Dir, name: &OsStr, directory: &Dir) -> io::Result<()> {
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    {
        rustix::fs::renameat_with(
            parent,
            name,
            directory,
            super::STAGED_FILE,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(io::Error::from)
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
    {
        let _ = (parent, name, directory);
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_rename_never_replaces_an_existing_file() {
        let root =
            std::env::temp_dir().join(format!("keeppeek-rename-{:032x}", rand::random::<u128>()));
        std::fs::create_dir(&root).unwrap();
        let directory = Dir::open_ambient_dir(&root, cap_std::ambient_authority()).unwrap();
        directory.write("source.mp4", [42; 8]).unwrap();
        directory.write(super::super::STAGED_FILE, [24; 8]).unwrap();
        let result = rename(&directory, OsStr::new("source.mp4"), &directory);
        let source = directory.read("source.mp4");
        let destination = directory.read(super::super::STAGED_FILE);
        drop(directory);
        std::fs::remove_dir_all(root).unwrap();

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(source.unwrap(), [42; 8]);
        assert_eq!(destination.unwrap(), [24; 8]);
    }
}
