use crate::storage::layout;
use std::path::{Path, PathBuf};

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

            if date_str < cutoff_date.as_str() {
                let path = entry.path();
                if path.is_dir() {
                    removed += remove_dir_count(&path)?;
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
        while remaining > max_bytes {
            let oldest = match self.find_oldest_date_dir() {
                Some(entry) => entry,
                None => break,
            };

            let freed = dir_size(&oldest).unwrap_or_default();
            let files = collect_finalized(&oldest).unwrap_or_default();

            if let Err(e) = std::fs::remove_dir_all(&oldest) {
                tracing::error!(path = %oldest.display(), error = %e, "failed to remove old date dir");
                break;
            }

            remaining = remaining.saturating_sub(freed);
            removed_files.extend(files);
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

    fn find_oldest_date_dir(&self) -> Option<PathBuf> {
        let mut oldest: Option<(String, PathBuf)> = None;
        find_oldest_date_dir(&self.root, &mut oldest);

        oldest.map(|(_, path)| path)
    }
}

fn find_oldest_date_dir(root: &Path, oldest: &mut Option<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if is_recording_date(name) {
            let key = name.to_owned();
            if oldest.as_ref().is_none_or(|(current, _)| key < *current) {
                *oldest = Some((key, path));
            }
        } else if !name.starts_with('.') {
            find_oldest_date_dir(&path, oldest);
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

fn collect_finalized(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            result.extend(collect_finalized(&path)?);
        } else if path.is_file() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.ends_with(".active") {
                result.push(path);
            }
        }
    }
    Ok(result)
}

fn remove_dir_count(dir: &Path) -> std::io::Result<u64> {
    let mut count = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            count += remove_dir_count(&path)?;
        } else {
            count += 1;
        }
    }
    std::fs::remove_dir_all(dir)?;
    Ok(count)
}

fn dir_size(dir: &Path) -> std::io::Result<u64> {
    let mut total = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            total += dir_size(&path)?;
        } else {
            total += entry.metadata()?.len();
        }
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
