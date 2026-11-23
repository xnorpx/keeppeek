use crate::storage::layout;
use cap_fs_ext::DirExt;
use cap_std::fs::Dir;
use std::path::{Path, PathBuf};

pub mod inspection;

pub struct LongTermStore {
    root: PathBuf,
}

impl LongTermStore {
    pub const fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn finalized_segments(&self, camera_id: &str) -> std::io::Result<Vec<PathBuf>> {
        let cam_dir = layout::camera_dir(&self.root, camera_id);
        if !cam_dir.exists() {
            return Ok(Vec::new());
        }
        collect_finalized(&cam_dir)
    }

    pub fn purge_before(
        &self,
        camera_id: &str,
        before: time::OffsetDateTime,
    ) -> std::io::Result<u64> {
        let cam_dir = layout::camera_dir(&self.root, camera_id);
        if !cam_dir.exists() {
            return Ok(0);
        }

        let cutoff_date = format!(
            "{:04}-{:02}-{:02}",
            before.year(),
            before.month() as u8,
            before.day()
        );

        let mut removed = 0u64;
        for entry in std::fs::read_dir(&cam_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(date_str) = name.to_str() else {
                continue;
            };

            if is_recording_date(date_str) && date_str < cutoff_date.as_str() {
                let path = entry.path();
                if entry.file_type()?.is_dir() {
                    self.prune_date(&path, &mut |_, _| removed += 1)?;
                }
            }
        }
        Ok(removed)
    }

    pub fn total_bytes(&self, camera_id: &str) -> std::io::Result<u64> {
        let cam_dir = layout::camera_dir(&self.root, camera_id);
        if !cam_dir.exists() {
            return Ok(0);
        }
        dir_size(&cam_dir)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn remove_catalog_recording(&self, path: &Path) -> std::io::Result<u64> {
        let relative = path.strip_prefix(&self.root).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "catalog recording is outside the configured archive root",
            )
        })?;
        if relative.components().any(|component| {
            component.as_os_str() == ".exports"
                || component.as_os_str() == layout::HEADER_RECOVERY_DIRECTORY
        }) || path.extension().and_then(|extension| extension.to_str()) != Some("mp4")
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "catalog recording is not eligible for automatic cleanup",
            ));
        }
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "catalog recording cleanup refuses links and non-files",
            ));
        }
        let canonical_root = self.root.canonicalize()?;
        let canonical_path = path.canonicalize()?;
        if !canonical_path.starts_with(&canonical_root) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "catalog recording resolves outside the configured archive root",
            ));
        }
        let bytes = metadata.len();
        std::fs::remove_file(path)?;
        if let Some(parent) = path.parent() {
            let _ = remove_empty_parents(parent, &self.root);
        }
        Ok(bytes)
    }

    pub fn enforce_limit(&self, max_bytes: u64) {
        let _ = self.enforce_limit_with_removed(max_bytes);
    }

    pub(crate) fn enforce_limit_with_removed(&self, max_bytes: u64) -> Vec<PathBuf> {
        let total = match dir_size(&self.root) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "failed to compute long-term storage size");
                return Vec::new();
            }
        };

        if total <= max_bytes {
            return Vec::new();
        }

        tracing::info!(
            total_gb = total / 1_073_741_824,
            max_gb = max_bytes / 1_073_741_824,
            "long-term storage over limit, pruning oldest data",
        );

        let mut remaining = total;
        let mut removed_files = Vec::new();
        let mut dates = Vec::new();
        collect_date_dirs(&self.root, &mut dates);
        dates.sort_unstable();
        for (_, oldest) in dates {
            if remaining <= max_bytes {
                break;
            }
            let mut freed = 0_u64;
            let result = self.prune_date(&oldest, &mut |path, bytes| {
                freed = freed.saturating_add(bytes);
                if is_finalized(path) {
                    removed_files.push(path.to_owned());
                }
            });
            remaining = remaining.saturating_sub(freed);
            if let Err(error) = result {
                tracing::error!(path = %oldest.display(), %error, "failed to prune old date directory");
                break;
            }
            tracing::info!(
                removed = %oldest.display(),
                freed_mb = freed / (1024 * 1024),
                remaining_gb = remaining / 1_073_741_824,
                "pruned old recordings",
            );

            if let Some(parent) = oldest.parent() {
                let _ = remove_empty_parents(parent, &self.root);
            }
        }
        removed_files
    }

    fn prune_date(&self, date: &Path, removed: &mut impl FnMut(&Path, u64)) -> std::io::Result<()> {
        let relative = date
            .strip_prefix(&self.root)
            .map_err(std::io::Error::other)?;
        let mut parent = Dir::open_ambient_dir(&self.root, cap_std::ambient_authority())?;
        let mut components = relative.components().peekable();
        while let Some(component) = components.next() {
            let std::path::Component::Normal(name) = component else {
                return Err(std::io::ErrorKind::InvalidInput.into());
            };
            if components.peek().is_none() {
                return prune_directory(&parent, name, date, removed);
            }
            parent = parent.open_dir_nofollow(name)?;
        }
        Err(std::io::ErrorKind::InvalidInput.into())
    }
}

fn collect_date_dirs(root: &Path, dates: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if is_recording_date(name) {
            dates.push((name.to_owned(), entry.path()));
        } else if !name.starts_with('.') {
            collect_date_dirs(&entry.path(), dates);
        }
    }
}

fn is_recording_date(value: &str) -> bool {
    value.len() == 10
        && value.as_bytes().get(4) == Some(&b'-')
        && value.as_bytes().get(7) == Some(&b'-')
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
}

fn is_finalized(path: &Path) -> bool {
    !path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .ends_with(".active")
}

fn collect_finalized(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name() == layout::HEADER_RECOVERY_DIRECTORY {
            continue;
        }
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            result.extend(collect_finalized(&path)?);
        } else if kind.is_file() && is_finalized(&path) {
            result.push(path);
        }
    }
    Ok(result)
}

/// Remove ordinary entries individually. A private recovery directory may appear
/// after enumeration, so remove only an empty parent and preserve any newly arrived entries.
fn prune_directory(
    parent: &Dir,
    name: &std::ffi::OsStr,
    path: &Path,
    removed: &mut impl FnMut(&Path, u64),
) -> std::io::Result<()> {
    let directory = parent.open_dir_nofollow(name)?;
    for entry in directory.entries()? {
        let entry = entry?;
        let leaf = entry.file_name();
        if leaf == layout::HEADER_RECOVERY_DIRECTORY {
            continue;
        }
        let kind = entry.file_type()?;
        let child = path.join(&leaf);
        if kind.is_dir() {
            prune_directory(&directory, &leaf, &child, removed)?;
        } else {
            let bytes = directory.symlink_metadata(&leaf)?.len();
            directory.remove_file(&leaf)?;
            removed(&child, bytes);
        }
    }
    drop(directory);
    match parent.remove_dir(name) {
        Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => Ok(()),
        result => result,
    }
}

fn dir_size(dir: &Path) -> std::io::Result<u64> {
    let mut total = 0_u64;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name() == layout::HEADER_RECOVERY_DIRECTORY {
            continue;
        }
        let bytes = if entry.file_type()?.is_dir() {
            dir_size(&entry.path())?
        } else {
            std::fs::symlink_metadata(entry.path())?.len()
        };
        total = total.saturating_add(bytes);
    }
    Ok(total)
}

fn remove_empty_parents(dir: &Path, stop_at: &Path) -> std::io::Result<()> {
    let mut current = dir;
    while current != stop_at {
        let is_empty = std::fs::read_dir(current)?.next().is_none();
        if !is_empty {
            break;
        }
        std::fs::remove_dir(current)?;
        current = match current.parent() {
            Some(p) => p,
            None => break,
        };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_entries_stay_private_and_survive_date_and_capacity_pruning() -> std::io::Result<()>
    {
        use std::time::{Duration, Instant};
        let root = std::env::temp_dir().join(format!(
            "keeppeek-long-term-recovery-{}",
            uuid::Uuid::new_v4()
        ));
        let old = root.join("camera/2026-08-20/00");
        let new = root.join("camera/2026-08-21/00");
        let recovery = old.join(layout::HEADER_RECOVERY_DIRECTORY);
        let pins = recovery.join("0000000.mp4.active.pins-v2");
        std::fs::create_dir_all(&pins)?;
        std::fs::create_dir_all(&new)?;
        let pin = pins.join("pin-0000000000000001-0000000000000002");
        let staged = recovery.join("0000000.mp4.active");
        std::fs::write(&pin, b"partial")?;
        std::fs::write(&staged, b"conflict")?;
        let old_file = old.join("0000000.mp4");
        let new_file = new.join("0000000.mp4");
        std::fs::write(&old_file, b"old")?;
        std::fs::write(&new_file, b"new")?;

        let store = LongTermStore::new(root.clone());
        let mut finalized = store.finalized_segments("camera")?;
        finalized.sort();
        assert_eq!(finalized, [old_file, new_file.clone()]);
        let (inventory, complete) = inspection::Archive::open(&root)?
            .inventory(Instant::now() + Duration::from_secs(10))?;
        assert!(complete);
        assert_eq!(inventory.len(), 2);
        assert!(
            inventory
                .iter()
                .all(|(path, active)| !active && finalized.contains(path))
        );
        assert_eq!(store.total_bytes("camera")?, 6);
        let cutoff = time::macros::datetime!(2026-08-21 00:00 UTC);
        assert_eq!(store.purge_before("camera", cutoff)?, 1);
        assert_eq!(store.enforce_limit_with_removed(0), [new_file]);
        assert_eq!(std::fs::read(pin)?, b"partial");
        assert_eq!(std::fs::read(staged)?, b"conflict");
        assert!(store.finalized_segments("camera")?.is_empty());
        std::fs::remove_dir_all(root)
    }

    #[test]
    fn prunes_oldest_date_below_camera_and_stream_directories() {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-long-term-prune-{}",
            rand::random::<u64>()
        ));
        let old = root.join("front_gate/main/2026-08-20/00");
        let new = root.join("front_gate/sub/2026-08-21/00");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(old.join("old.mp4"), vec![1; 16]).unwrap();
        std::fs::write(new.join("new.mp4"), vec![2; 16]).unwrap();

        let store = LongTermStore::new(root.clone());
        let removed = store.enforce_limit_with_removed(16);

        assert_eq!(removed, vec![old.join("old.mp4")]);
        assert!(!old.exists());
        assert!(new.join("new.mp4").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
