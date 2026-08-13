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
        let total = match dir_size(&self.root) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "failed to compute long-term storage size");
                return;
            }
        };

        if total <= max_bytes {
            return;
        }

        tracing::info!(
            total_gb = total / 1_073_741_824,
            max_gb = max_bytes / 1_073_741_824,
            "long-term storage over limit, pruning oldest data",
        );

        let mut remaining = total;
        while remaining > max_bytes {
            let oldest = match self.find_oldest_date_dir() {
                Some(entry) => entry,
                None => break,
            };

            let freed = dir_size(&oldest).unwrap_or_default();

            if let Err(e) = std::fs::remove_dir_all(&oldest) {
                tracing::error!(path = %oldest.display(), error = %e, "failed to remove old date dir");
                break;
            }

            remaining = remaining.saturating_sub(freed);
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
    }

    fn find_oldest_date_dir(&self) -> Option<PathBuf> {
        let cameras = std::fs::read_dir(&self.root).ok()?;
        let mut oldest: Option<(String, PathBuf)> = None;

        for cam_entry in cameras.flatten() {
            let cam_path = cam_entry.path();
            if !cam_path.is_dir() {
                continue;
            }
            let dates = match std::fs::read_dir(&cam_path) {
                Ok(d) => d,
                Err(_) => continue,
            };
            for date_entry in dates.flatten() {
                let name = date_entry.file_name();
                let Some(date_str) = name.to_str() else {
                    continue;
                };
                if !date_entry.path().is_dir() || date_str.len() != 10 {
                    continue;
                }
                let key = date_str.to_owned();
                let dominated = oldest.as_ref().is_none_or(|(cur, _)| key < *cur);
                if dominated {
                    oldest = Some((key, date_entry.path()));
                }
            }
        }

        oldest.map(|(_, path)| path)
    }
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
