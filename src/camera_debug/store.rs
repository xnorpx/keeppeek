//! Bounded disk storage, TTL/FIFO eviction, atomic metadata, and concurrency guards.
//!
//! Stores camera debug captures under `<config-dir>/camera-debug/<capture-id>/`,
//! enforcing per-capture and total storage limits, 24-hour TTL eviction, strict
//! `capture_id` validation against path traversal, and at most one concurrent
//! capture per `camera_id`.

use crate::camera_debug::bitstream::BitstreamAnomalyReport;
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

/// Default maximum disk size for a single capture (64 MiB).
pub const DEFAULT_MAX_CAPTURE_BYTES: u64 = 64 * 1024 * 1024;
/// Default maximum aggregate disk size across all captures (512 MiB).
pub const DEFAULT_MAX_STORAGE_BYTES: u64 = 512 * 1024 * 1024;
/// Default retention time before captures expire (24 hours).
pub const DEFAULT_CAPTURE_TTL_SECS: u64 = 24 * 60 * 60;
/// Maximum permitted `capture_id` length in ASCII bytes.
pub const CAPTURE_ID_LEN_MAX: usize = 128;
/// Maximum permitted `camera_id` length in bytes.
pub const CAMERA_ID_LEN_MAX: usize = 128;
/// Maximum permitted single-segment artifact filename length in bytes.
pub const ARTIFACT_NAME_LEN_MAX: usize = 128;
/// Metadata filename written inside each capture directory.
pub const CAPTURE_METADATA_FILENAME: &str = "capture.json";

/// Bounds governing [`CaptureStore`] retention and size limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreLimits {
    pub max_capture_bytes: u64,
    pub max_storage_bytes: u64,
    pub ttl_secs: u64,
}

impl Default for StoreLimits {
    fn default() -> Self {
        Self {
            max_capture_bytes: DEFAULT_MAX_CAPTURE_BYTES,
            max_storage_bytes: DEFAULT_MAX_STORAGE_BYTES,
            ttl_secs: DEFAULT_CAPTURE_TTL_SECS,
        }
    }
}

/// Persisted metadata describing one capture stored on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureMetadata {
    pub capture_id: String,
    pub camera_id: String,
    pub protocol: String,
    pub created_at_unix_secs: u64,
    pub duration_ms: u64,
    pub total_bytes: u64,
    pub packet_count: u64,
    pub include_raw_auth: bool,
    pub trigger_probe: bool,
    pub tls_decrypted: bool,
    pub transport: String,
    pub files: Vec<String>,
    pub anomalies: BitstreamAnomalyReport,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Summary of captures deleted during a pruning pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneReport {
    pub expired_deleted: usize,
    pub evicted_fifo: usize,
    pub remaining_bytes: u64,
}

fn active_capture_registry() -> &'static Mutex<HashSet<(PathBuf, String)>> {
    static ACTIVE: OnceLock<Mutex<HashSet<(PathBuf, String)>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashSet::new()))
}

/// RAII guard enforcing at most one active capture per `(store_root, camera_id)`.
#[derive(Debug)]
pub struct ActiveCaptureGuard {
    root_dir: PathBuf,
    camera_id: String,
}

impl ActiveCaptureGuard {
    fn acquire(root_dir: PathBuf, camera_id: &str) -> anyhow::Result<Self> {
        validate_camera_id(camera_id)?;
        let key = (root_dir.clone(), camera_id.to_owned());
        let mut guard = active_capture_registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ensure!(
            !guard.contains(&key),
            "capture already active for camera '{camera_id}'"
        );
        guard.insert(key);
        Ok(Self {
            root_dir,
            camera_id: camera_id.to_owned(),
        })
    }

    /// Returns the camera identifier protected by this guard.
    #[must_use]
    pub fn camera_id(&self) -> &str {
        &self.camera_id
    }
}

impl Drop for ActiveCaptureGuard {
    fn drop(&mut self) {
        let mut guard = active_capture_registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.remove(&(self.root_dir.clone(), self.camera_id.clone()));
    }
}

/// Bounded local disk store for camera debug capture directories.
#[derive(Debug, Clone)]
pub struct CaptureStore {
    root_dir: PathBuf,
    limits: StoreLimits,
}

impl CaptureStore {
    /// Opens the default store under `<config-dir>/camera-debug`.
    ///
    /// # Errors
    /// Returns an error if the store directory cannot be created.
    pub fn open_default(limits: StoreLimits) -> anyhow::Result<Self> {
        let root_dir = crate::config::config_dir().join("camera-debug");
        Self::new(root_dir, limits)
    }

    /// Opens or creates a bounded store rooted at `root_dir`.
    ///
    /// # Errors
    /// Returns an error if limits are invalid or `root_dir` cannot be created.
    pub fn new(root_dir: impl AsRef<Path>, limits: StoreLimits) -> anyhow::Result<Self> {
        ensure!(
            limits.max_capture_bytes > 0,
            "max_capture_bytes must be > 0"
        );
        ensure!(
            limits.max_storage_bytes >= limits.max_capture_bytes,
            "max_storage_bytes must be >= max_capture_bytes"
        );
        ensure!(limits.ttl_secs > 0, "ttl_secs must be > 0");
        let root_dir = root_dir.as_ref().to_path_buf();
        ensure_private_dir(&root_dir)?;
        Ok(Self { root_dir, limits })
    }

    /// Returns the store root directory path.
    #[must_use]
    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    /// Returns the configured storage limits.
    #[must_use]
    pub const fn limits(&self) -> StoreLimits {
        self.limits
    }

    /// Validates `capture_id` and resolves its directory inside the store root.
    ///
    /// # Errors
    /// Returns an error if `capture_id` is empty, oversized, or contains unsafe path tokens.
    pub fn capture_dir(&self, capture_id: &str) -> anyhow::Result<PathBuf> {
        validate_capture_id(capture_id)?;
        let path = self.root_dir.join(capture_id);
        ensure!(
            path.parent() == Some(self.root_dir.as_path()),
            "capture path escaped store root"
        );
        Ok(path)
    }

    /// Returns `true` if a capture is currently active for `camera_id` in this store.
    #[must_use]
    pub fn is_camera_capture_active(&self, camera_id: &str) -> bool {
        let guard = active_capture_registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.contains(&(self.root_dir.clone(), camera_id.to_owned()))
    }

    /// Prunes expired captures, acquires the per-camera concurrency guard, and creates the directory.
    ///
    /// # Errors
    /// Returns an error if another capture is active for `camera_id`, `capture_id` already exists,
    /// or directory creation fails.
    pub fn begin_capture(
        &self,
        camera_id: &str,
        capture_id: &str,
    ) -> anyhow::Result<(ActiveCaptureGuard, PathBuf)> {
        let guard = ActiveCaptureGuard::acquire(self.root_dir.clone(), camera_id)?;
        let dir = self.capture_dir(capture_id)?;
        ensure!(!dir.exists(), "capture '{capture_id}' already exists");
        self.prune_at(unix_now_secs(), Some(capture_id))?;
        ensure_private_dir(&dir)?;
        Ok((guard, dir))
    }

    /// Writes `capture.json` atomically using owner-only file permissions.
    ///
    /// # Errors
    /// Returns an error if `capture_id` is invalid, metadata exceeds `max_capture_bytes`, or I/O fails.
    pub fn save_metadata(&self, capture_id: &str, meta: &CaptureMetadata) -> anyhow::Result<()> {
        ensure!(
            meta.capture_id == capture_id,
            "metadata capture_id '{}' does not match '{capture_id}'",
            meta.capture_id
        );
        let dir = self.capture_dir(capture_id)?;
        ensure_private_dir(&dir)?;
        let json = serde_json::to_vec_pretty(meta)?;
        let target = dir.join(CAPTURE_METADATA_FILENAME);
        write_private_atomically(&target, &json)?;
        Ok(())
    }

    /// Updates `meta.total_bytes` from disk, enforces `max_capture_bytes`, saves metadata, and prunes.
    ///
    /// # Errors
    /// Returns an error if the capture exceeds `max_capture_bytes` or metadata writing fails.
    pub fn finalize_capture(
        &self,
        capture_id: &str,
        meta: &mut CaptureMetadata,
    ) -> anyhow::Result<()> {
        let dir = self.capture_dir(capture_id)?;
        let bytes = dir_size_bytes(&dir)?;
        ensure!(
            bytes <= self.limits.max_capture_bytes,
            "capture '{capture_id}' size ({bytes} bytes) exceeds max_capture_bytes ({})",
            self.limits.max_capture_bytes
        );
        meta.total_bytes = bytes;
        self.save_metadata(capture_id, meta)?;
        meta.total_bytes = dir_size_bytes(&dir)?;
        self.save_metadata(capture_id, meta)?;
        self.prune_at(unix_now_secs(), Some(capture_id))?;
        Ok(())
    }

    /// Reads and parses `capture.json` for `capture_id`.
    ///
    /// # Errors
    /// Returns an error if `capture_id` is invalid or metadata is missing or corrupt.
    pub fn load_metadata(&self, capture_id: &str) -> anyhow::Result<CaptureMetadata> {
        let path = self
            .capture_dir(capture_id)?
            .join(CAPTURE_METADATA_FILENAME);
        let bytes = std::fs::read(&path)
            .with_context(|| format!("unable to read capture metadata {}", path.display()))?;
        let meta: CaptureMetadata = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid capture metadata {}", path.display()))?;
        ensure!(meta.capture_id == capture_id, "capture_id mismatch on disk");
        Ok(meta)
    }

    /// Lists non-expired captures sorted newest first.
    ///
    /// # Errors
    /// Returns an error if the store directory cannot be read.
    pub fn list_captures(&self) -> anyhow::Result<Vec<CaptureMetadata>> {
        self.prune_at(unix_now_secs(), None)?;
        let mut items = self.scan_captures()?;
        items.sort_by(|a, b| {
            b.created_at_unix_secs
                .cmp(&a.created_at_unix_secs)
                .then_with(|| b.capture_id.cmp(&a.capture_id))
        });
        Ok(items)
    }

    /// Deletes a capture directory by `capture_id`, returning `true` if it existed.
    ///
    /// # Errors
    /// Returns an error if `capture_id` is invalid or removal fails.
    pub fn delete_capture(&self, capture_id: &str) -> anyhow::Result<bool> {
        let dir = self.capture_dir(capture_id)?;
        if !dir.exists() {
            return Ok(false);
        }
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("unable to delete capture {}", dir.display()))?;
        Ok(true)
    }

    /// Enforces TTL expiration and FIFO storage quota as of `now_unix_secs`.
    ///
    /// # Errors
    /// Returns an error if scanning or deleting capture directories fails.
    pub fn prune_at(
        &self,
        now_unix_secs: u64,
        preserve_capture_id: Option<&str>,
    ) -> anyhow::Result<PruneReport> {
        let entries = self.scan_entries()?;
        let mut report = PruneReport::default();
        let mut retained = Vec::new();
        for entry in entries {
            let is_preserved = preserve_capture_id == Some(entry.capture_id.as_str());
            let expired = !is_preserved
                && now_unix_secs.saturating_sub(entry.created_at) >= self.limits.ttl_secs;
            if expired {
                std::fs::remove_dir_all(&entry.path)?;
                report.expired_deleted += 1;
            } else {
                retained.push(entry);
            }
        }
        retained.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.capture_id.cmp(&b.capture_id))
        });
        let mut total_bytes: u64 = retained.iter().map(|entry| entry.size_bytes).sum();
        for entry in retained {
            if total_bytes <= self.limits.max_storage_bytes {
                break;
            }
            if preserve_capture_id == Some(entry.capture_id.as_str()) {
                continue;
            }
            std::fs::remove_dir_all(&entry.path)?;
            total_bytes = total_bytes.saturating_sub(entry.size_bytes);
            report.evicted_fifo += 1;
        }
        report.remaining_bytes = total_bytes;
        Ok(report)
    }

    fn scan_captures(&self) -> anyhow::Result<Vec<CaptureMetadata>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.root_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if validate_capture_id(&name).is_err() {
                continue;
            }
            if let Ok(meta) = self.load_metadata(&name) {
                out.push(meta);
            }
        }
        Ok(out)
    }

    fn scan_entries(&self) -> anyhow::Result<Vec<StoredEntry>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.root_dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if !file_type.is_dir() {
                continue;
            }
            let Some(capture_id) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if validate_capture_id(&capture_id).is_err() {
                continue;
            }
            let path = entry.path();
            let size_bytes = dir_size_bytes(&path)?;
            let created_at = self
                .load_metadata(&capture_id)
                .map(|meta| meta.created_at_unix_secs)
                .unwrap_or_else(|_| dir_modified_unix_secs(&path));
            out.push(StoredEntry {
                capture_id,
                path,
                size_bytes,
                created_at,
            });
        }
        Ok(out)
    }
}

struct StoredEntry {
    capture_id: String,
    path: PathBuf,
    size_bytes: u64,
    created_at: u64,
}

/// Validates that `capture_id` is a safe ASCII identifier without path traversal segments.
///
/// # Errors
/// Returns an error when `capture_id` is empty, exceeds 128 bytes, or contains characters
/// outside `[a-zA-Z0-9_-]`.
pub fn validate_capture_id(capture_id: &str) -> anyhow::Result<()> {
    ensure!(
        !capture_id.is_empty() && capture_id.len() <= CAPTURE_ID_LEN_MAX,
        "capture_id must be between 1 and {CAPTURE_ID_LEN_MAX} bytes"
    );
    ensure!(
        capture_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')),
        "capture_id '{capture_id}' contains disallowed characters"
    );
    Ok(())
}

/// Validates that `camera_id` is non-empty, bounded, and free of control characters.
///
/// # Errors
/// Returns an error if `camera_id` is empty, exceeds 128 bytes, or has control characters.
pub fn validate_camera_id(camera_id: &str) -> anyhow::Result<()> {
    ensure!(
        !camera_id.is_empty() && camera_id.len() <= CAMERA_ID_LEN_MAX,
        "camera_id must be between 1 and {CAMERA_ID_LEN_MAX} bytes"
    );
    ensure!(
        !camera_id.chars().any(char::is_control),
        "camera_id must not contain control characters"
    );
    Ok(())
}

/// Validates that `artifact` is a single-segment filename without path traversal tokens.
///
/// # Errors
/// Returns an error if `artifact` is empty, exceeds 128 bytes, or contains `/`, `\`, `..`, NUL, or control chars.
pub fn validate_artifact_name(artifact: &str) -> anyhow::Result<()> {
    ensure!(
        !artifact.is_empty() && artifact.len() <= ARTIFACT_NAME_LEN_MAX,
        "artifact filename must be between 1 and {ARTIFACT_NAME_LEN_MAX} bytes"
    );
    ensure!(
        artifact != "." && artifact != ".." && !artifact.contains(".."),
        "artifact filename '{artifact}' contains disallowed path traversal segment"
    );
    ensure!(
        !artifact
            .bytes()
            .any(|byte| matches!(byte, b'/' | b'\\' | b'\0') || byte.is_ascii_control())
            && !artifact.chars().any(char::is_control),
        "artifact filename '{artifact}' contains disallowed path separator or control character"
    );
    let mut components = Path::new(artifact).components();
    ensure!(
        matches!(components.next(), Some(std::path::Component::Normal(_)))
            && components.next().is_none(),
        "artifact filename '{artifact}' must be a single relative path segment"
    );
    Ok(())
}

fn dir_size_bytes(dir: &Path) -> anyhow::Result<u64> {
    let mut total = 0_u64;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_file() {
            total = total.saturating_add(meta.len());
        } else if meta.is_dir() {
            total = total.saturating_add(dir_size_bytes(&entry.path())?);
        }
    }
    Ok(total)
}

fn dir_modified_unix_secs(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |dur| dur.as_secs())
}

fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn ensure_private_dir(path: &Path) -> std::io::Result<()> {
    let existed = path.exists();
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    if !existed {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    let _ = existed;
    Ok(())
}

fn write_private_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let temp = parent.join(format!(".capture.{}.tmp", uuid::Uuid::new_v4()));
    let write_res = (|| {
        let mut file = crate::backup::create_private_file(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if let Err(error) = write_res {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    if let Err(error) = replace_file_atomically(&temp, path, parent) {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    Ok(())
}

fn replace_file_atomically(temp: &Path, target: &Path, parent: &Path) -> std::io::Result<()> {
    std::fs::rename(temp, target)?;
    #[cfg(unix)]
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    #[cfg(not(unix))]
    let _ = parent;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("keeppeek-store-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn sample_meta(id: &str, camera: &str, created_at: u64) -> CaptureMetadata {
        CaptureMetadata {
            capture_id: id.to_owned(),
            camera_id: camera.to_owned(),
            protocol: "rtsp".to_owned(),
            created_at_unix_secs: created_at,
            duration_ms: 500,
            total_bytes: 0,
            packet_count: 10,
            include_raw_auth: true,
            trigger_probe: false,
            tls_decrypted: false,
            transport: "tcp".to_owned(),
            files: vec!["capture.pcapng".to_owned()],
            anomalies: BitstreamAnomalyReport::default(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn rejects_path_traversal_and_concurrent_camera_capture() {
        let root = TempDir::new();
        let store = CaptureStore::new(root.path(), StoreLimits::default()).unwrap();
        for bad in ["", ".", "..", "../etc", "a/b", "a\\b", "foo..bar", "cap 1"] {
            assert!(validate_capture_id(bad).is_err(), "expected reject: {bad}");
            assert!(store.capture_dir(bad).is_err());
        }

        let (guard, dir) = store.begin_capture("cam-1", "cap_001").unwrap();
        assert!(dir.exists());
        assert!(store.begin_capture("cam-1", "cap_002").is_err());
        let (_other, _other_dir) = store.begin_capture("cam-2", "cap_003").unwrap();
        drop(guard);
        let (_reacquired, _dir2) = store.begin_capture("cam-1", "cap_004").unwrap();
    }

    #[test]
    fn enforces_max_capture_bytes_ttl_and_fifo_eviction() {
        let root = TempDir::new();
        let now = unix_now_secs();
        let meta_len = serde_json::to_vec_pretty(&sample_meta("cap-mid", "cam-1", now))
            .unwrap()
            .len() as u64;
        let single_dir_bytes = 1500 + meta_len;
        let limits = StoreLimits {
            max_capture_bytes: single_dir_bytes + 512,
            max_storage_bytes: single_dir_bytes * 2 + 256,
            ttl_secs: 100,
        };
        let store = CaptureStore::new(root.path(), limits).unwrap();

        for (idx, created_at, payload_len) in [
            ("cap-expired", now - 80, 1200_usize),
            ("cap-old", now - 50, 1500_usize),
            ("cap-mid", now - 20, 1500_usize),
            ("cap-new", now - 5, 1500_usize),
        ] {
            let dir = store.capture_dir(idx).unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("capture.pcapng"), vec![0xAA; payload_len]).unwrap();
            let mut meta = sample_meta(idx, "cam-1", created_at);
            meta.total_bytes = payload_len as u64;
            store.save_metadata(idx, &meta).unwrap();
        }

        let prune = store.prune_at(now + 30, None).unwrap();
        assert_eq!(prune.expired_deleted, 1);
        assert_eq!(prune.evicted_fifo, 1);
        assert!(prune.remaining_bytes <= limits.max_storage_bytes);
        assert!(!store.capture_dir("cap-expired").unwrap().exists());
        assert!(!store.capture_dir("cap-old").unwrap().exists());
        assert!(store.capture_dir("cap-mid").unwrap().exists());
        assert!(store.capture_dir("cap-new").unwrap().exists());

        let mut mid_meta = store.load_metadata("cap-mid").unwrap();
        store.finalize_capture("cap-mid", &mut mid_meta).unwrap();
        assert!(mid_meta.total_bytes > 1500);

        let dir = store.capture_dir("cap-too-big").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("capture.pcapng"), vec![0xBB; 4096]).unwrap();
        let mut oversized = sample_meta("cap-too-big", "cam-1", now);
        assert!(
            store
                .finalize_capture("cap-too-big", &mut oversized)
                .is_err()
        );

        assert!(store.delete_capture("cap-mid").unwrap());
        assert!(!store.delete_capture("cap-mid").unwrap());
    }
}
