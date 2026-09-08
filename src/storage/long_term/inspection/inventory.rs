use super::{Archive, PATH_BYTES_MAX, PATH_COMPONENTS_MAX, check_deadline};
use cap_fs_ext::DirExt;
use std::{io, path::PathBuf, time::Instant};

const INVENTORY_MAX: usize = 4_096;

impl Archive {
    pub(in crate::storage) fn inventory(
        &self,
        deadline: Instant,
    ) -> io::Result<(Vec<(PathBuf, bool)>, bool)> {
        let mut pending = vec![(self.directory.try_clone()?, PathBuf::new(), 0_usize)];
        let mut files = Vec::new();
        let mut scanned = 0;
        let mut complete = true;
        while let Some((directory, relative, depth)) = pending.pop() {
            for entry in directory.entries()? {
                check_deadline(deadline)?;
                scanned += 1;
                if scanned > INVENTORY_MAX {
                    return Ok((files, false));
                }
                let entry = entry?;
                let name = entry.file_name();
                let child = relative.join(&name);
                if child.as_os_str().len() > PATH_BYTES_MAX {
                    complete = false;
                    continue;
                }
                if name == ".exports" || name == ".maintenance" {
                    continue;
                }
                let metadata = directory.symlink_metadata(&name)?;
                if metadata.is_dir() && !metadata.file_type().is_symlink() {
                    if depth + 1 >= PATH_COMPONENTS_MAX {
                        complete = false;
                        continue;
                    }
                    pending.push((directory.open_dir_nofollow(&name)?, child, depth + 1));
                } else if child
                    .extension()
                    .is_some_and(|extension| extension == "mp4" || extension == "active")
                {
                    let temporary = child
                        .extension()
                        .is_some_and(|extension| extension == "active");
                    files.push((self.root.join(child), temporary));
                }
            }
        }
        Ok((files, complete))
    }
}
