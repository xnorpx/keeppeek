//! Atomic storage helpers for KeepPeek `secrets.toml` files.
//!
//! This module validates secret key names and values, reads and mutates
//! `secrets.toml` under a shared lock, zeroizes temporary secret buffers, and
//! writes files atomically with owner-only permissions (`0600` on Unix and a
//! protected owner-only DACL on Windows).

use super::{DEFAULT_SECRETS_TEMPLATE, Secrets};
use anyhow::Context as _;
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, OnceLock},
    time::SystemTime,
};
use zeroize::Zeroizing;

/// Maximum length in bytes of a secret key name.
pub const SECRET_KEY_LEN_MAX: usize = 128;
/// Maximum length in bytes of a secret value.
pub const SECRET_VALUE_BYTES_MAX: usize = 4 * 1024;
/// Maximum number of entries permitted in one `secrets.toml` file.
pub const SECRET_ENTRIES_MAX: usize = 1_024;
/// Maximum size in bytes read from an existing `secrets.toml` file.
pub const SECRETS_FILE_BYTES_MAX: u64 = 1024 * 1024;

/// File metadata snapshot used to detect on-disk `secrets.toml` updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecretsFileFingerprint {
    /// File length in bytes.
    pub len_bytes: u64,
    /// Last modification timestamp when supported by the filesystem.
    pub modified: Option<SystemTime>,
}

/// Returns the process-wide configuration lock used by standalone callers.
///
/// Server code that already holds `ServerState::config_update` should pass that
/// lock to the `*_with_lock` functions instead.
pub fn shared_config_lock() -> &'static Mutex<()> {
    // ponytail: single process-wide lock; per-path locks are only needed if one
    // process serves multiple independent config directories concurrently.
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Validates that `key` matches `^[A-Z_][A-Z0-9_]*$` within `1..=128` bytes.
///
/// # Errors
///
/// Returns an error when `key` is empty, exceeds [`SECRET_KEY_LEN_MAX`] bytes,
/// or contains disallowed characters.
pub fn validate_secret_key(key: &str) -> anyhow::Result<()> {
    if key.is_empty() || key.len() > SECRET_KEY_LEN_MAX {
        anyhow::bail!("secret key must be between 1 and {SECRET_KEY_LEN_MAX} ASCII bytes");
    }
    let mut bytes = key.bytes();
    let first_valid = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_uppercase() || byte == b'_');
    let rest_valid =
        bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_');
    if !first_valid || !rest_valid {
        anyhow::bail!(
            "invalid secret key '{key}'; use uppercase ASCII letters, digits, and underscores"
        );
    }
    Ok(())
}

/// Validates that `value` is non-empty, within 4 KiB, and free of NUL bytes.
///
/// # Errors
///
/// Returns an error when `value` is empty, exceeds [`SECRET_VALUE_BYTES_MAX`]
/// bytes, or contains a NUL byte.
pub fn validate_secret_value(value: &str) -> anyhow::Result<()> {
    if value.is_empty() {
        anyhow::bail!("secret value must not be empty");
    }
    if value.len() > SECRET_VALUE_BYTES_MAX {
        anyhow::bail!("secret value must not exceed {SECRET_VALUE_BYTES_MAX} bytes");
    }
    if value.bytes().any(|byte| byte == 0) {
        anyhow::bail!("secret value must not contain NUL bytes");
    }
    Ok(())
}

/// Inserts or updates `key` in `secrets_path` atomically under the shared lock.
///
/// # Errors
///
/// Returns an error when `key` or `value` is invalid, the entry cap is
/// exceeded, the existing file cannot be parsed, or the atomic write fails.
pub fn set_secret_entry(secrets_path: &Path, key: &str, value: &str) -> anyhow::Result<()> {
    set_secret_entry_with_lock(secrets_path, shared_config_lock(), key, value)
}

/// Inserts or updates `key` in `secrets_path` atomically under `config_lock`.
///
/// # Errors
///
/// Returns an error when `key` or `value` is invalid, the entry cap is
/// exceeded, the existing file cannot be parsed, or the atomic write fails.
pub fn set_secret_entry_with_lock(
    secrets_path: &Path,
    config_lock: &Mutex<()>,
    key: &str,
    value: &str,
) -> anyhow::Result<()> {
    validate_secret_key(key)?;
    validate_secret_value(value)?;
    let _guard = lock_mutex(config_lock);
    let mut entries = read_secret_entries_locked(secrets_path)?;
    if !entries.contains_key(key) && entries.len() >= SECRET_ENTRIES_MAX {
        anyhow::bail!("secrets file cannot contain more than {SECRET_ENTRIES_MAX} entries");
    }
    entries.insert(key.to_owned(), Zeroizing::new(value.to_owned()));
    write_secret_entries_locked(secrets_path, &entries)
}

/// Lists sorted secret key names stored in `secrets_path` without returning values.
///
/// # Errors
///
/// Returns an error when `secrets_path` exists and cannot be read or parsed.
pub fn list_secret_keys(secrets_path: &Path) -> anyhow::Result<Vec<String>> {
    list_secret_keys_with_lock(secrets_path, shared_config_lock())
}

/// Lists sorted secret key names stored in `secrets_path` under `config_lock`.
///
/// # Errors
///
/// Returns an error when `secrets_path` exists and cannot be read or parsed.
pub fn list_secret_keys_with_lock(
    secrets_path: &Path,
    config_lock: &Mutex<()>,
) -> anyhow::Result<Vec<String>> {
    let _guard = lock_mutex(config_lock);
    let entries = read_secret_entries_locked(secrets_path)?;
    Ok(entries.into_keys().collect())
}

/// Removes `key` from `secrets_path` atomically under the shared lock.
///
/// Returns `Ok(true)` when the key existed and was removed, or `Ok(false)` when
/// the key or file did not exist.
///
/// # Errors
///
/// Returns an error when `key` is invalid, the existing file cannot be parsed,
/// or the atomic write fails.
pub fn remove_secret_entry(secrets_path: &Path, key: &str) -> anyhow::Result<bool> {
    remove_secret_entry_with_lock(secrets_path, shared_config_lock(), key)
}

/// Removes `key` from `secrets_path` atomically under `config_lock`.
///
/// Returns `Ok(true)` when the key existed and was removed, or `Ok(false)` when
/// the key or file did not exist.
///
/// # Errors
///
/// Returns an error when `key` is invalid, the existing file cannot be parsed,
/// or the atomic write fails.
pub fn remove_secret_entry_with_lock(
    secrets_path: &Path,
    config_lock: &Mutex<()>,
    key: &str,
) -> anyhow::Result<bool> {
    validate_secret_key(key)?;
    let _guard = lock_mutex(config_lock);
    if !secrets_path.exists() {
        return Ok(false);
    }
    let mut entries = read_secret_entries_locked(secrets_path)?;
    let removed = entries.remove(key).is_some();
    if removed {
        write_secret_entries_locked(secrets_path, &entries)?;
    }
    Ok(removed)
}

/// Loads `secrets_path` into a [`Secrets`] snapshot under the shared lock.
///
/// Returns an empty [`Secrets`] snapshot when `secrets_path` does not exist.
///
/// # Errors
///
/// Returns an error when `secrets_path` exceeds size bounds, cannot be read, or
/// contains invalid keys or non-string TOML values.
pub fn load_secrets_file(secrets_path: &Path) -> anyhow::Result<Secrets> {
    load_secrets_file_with_lock(secrets_path, shared_config_lock())
}

/// Loads `secrets_path` into a [`Secrets`] snapshot under `config_lock`.
///
/// Server components call this helper to pick up live `secrets.toml` updates
/// without restarting the process.
///
/// # Errors
///
/// Returns an error when `secrets_path` exceeds size bounds, cannot be read, or
/// contains invalid keys or non-string TOML values.
pub fn load_secrets_file_with_lock(
    secrets_path: &Path,
    config_lock: &Mutex<()>,
) -> anyhow::Result<Secrets> {
    let _guard = lock_mutex(config_lock);
    let entries = read_secret_entries_locked(secrets_path)?;
    let map = entries
        .into_iter()
        .map(|(key, value)| (key, (*value).clone()))
        .collect();
    Ok(Secrets(map))
}

/// Resolves a `{secret:KEY}` or `{secret:KEY|url}` reference against `secrets`.
///
/// # Errors
///
/// Returns an error when `reference` is malformed or the key is missing.
pub(crate) fn resolve_loaded_reference(
    reference: &str,
    secrets: &Secrets,
) -> anyhow::Result<Zeroizing<String>> {
    super::resolve_secret_references_loaded(reference, secrets).map(Zeroizing::new)
}

/// Reads metadata for `secrets_path` to detect on-disk changes.
///
/// Returns `Ok(None)` when `secrets_path` does not exist.
///
/// # Errors
///
/// Returns an error when the filesystem metadata query fails for another reason.
pub fn secrets_file_fingerprint(
    secrets_path: &Path,
) -> std::io::Result<Option<SecretsFileFingerprint>> {
    match std::fs::metadata(secrets_path) {
        Ok(metadata) => Ok(Some(SecretsFileFingerprint {
            len_bytes: metadata.len(),
            modified: metadata.modified().ok(),
        })),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn lock_mutex(lock: &Mutex<()>) -> MutexGuard<'_, ()> {
    lock.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn read_secret_entries_locked(
    secrets_path: &Path,
) -> anyhow::Result<BTreeMap<String, Zeroizing<String>>> {
    let file = match File::open(secrets_path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("unable to open {}", secrets_path.display()));
        }
    };
    let mut raw_bytes = Zeroizing::new(Vec::new());
    file.take(SECRETS_FILE_BYTES_MAX + 1)
        .read_to_end(&mut raw_bytes)
        .with_context(|| format!("unable to read {}", secrets_path.display()))?;
    if raw_bytes.len() as u64 > SECRETS_FILE_BYTES_MAX {
        anyhow::bail!(
            "secrets file {} exceeds {SECRETS_FILE_BYTES_MAX} bytes",
            secrets_path.display()
        );
    }
    let text = Zeroizing::new(String::from_utf8(std::mem::take(&mut *raw_bytes)).map_err(
        |_| {
            anyhow::anyhow!(
                "secrets file {} must be valid UTF-8",
                secrets_path.display()
            )
        },
    )?);
    parse_secret_table(secrets_path, &text)
}

fn parse_secret_table(
    secrets_path: &Path,
    text: &str,
) -> anyhow::Result<BTreeMap<String, Zeroizing<String>>> {
    let raw: BTreeMap<String, String> = toml::from_str(text).map_err(|_| {
        anyhow::anyhow!(
            "unable to parse {}; secrets must be a flat string-to-string TOML table",
            secrets_path.display()
        )
    })?;
    if raw.len() > SECRET_ENTRIES_MAX {
        anyhow::bail!(
            "secrets file {} cannot contain more than {SECRET_ENTRIES_MAX} entries",
            secrets_path.display()
        );
    }
    let mut entries = BTreeMap::new();
    for (key, value) in raw {
        let value = Zeroizing::new(value);
        validate_secret_key(&key)?;
        entries.insert(key, value);
    }
    Ok(entries)
}

fn write_secret_entries_locked(
    secrets_path: &Path,
    entries: &BTreeMap<String, Zeroizing<String>>,
) -> anyhow::Result<()> {
    let serialized = serialize_secret_entries(entries)?;
    write_private_atomically(secrets_path, serialized.as_bytes())
        .with_context(|| format!("unable to write {}", secrets_path.display()))
}

fn serialize_secret_entries(
    entries: &BTreeMap<String, Zeroizing<String>>,
) -> anyhow::Result<Zeroizing<String>> {
    if entries.is_empty() {
        return Ok(Zeroizing::new(DEFAULT_SECRETS_TEMPLATE.to_owned()));
    }
    let mut table = toml::Table::new();
    for (key, value) in entries {
        table.insert(key.clone(), toml::Value::String(value.as_str().to_owned()));
    }
    let rendered = toml::to_string_pretty(&table)?;
    Ok(Zeroizing::new(rendered))
}

fn write_private_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    ensure_private_parent(parent)?;
    let temp_path = temporary_secrets_path(parent, path);
    let write_result = write_and_sync_temp_file(&temp_path, bytes);
    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error);
    }
    if let Err(error) = replace_file_atomically(&temp_path, path, parent) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error);
    }
    Ok(())
}

fn ensure_private_parent(parent: &Path) -> std::io::Result<()> {
    let existed = parent.exists();
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    if !existed {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    let _ = existed;
    Ok(())
}

fn temporary_secrets_path(parent: &Path, target: &Path) -> PathBuf {
    let filename = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("secrets.toml");
    parent.join(format!(".{filename}.{}.tmp", uuid::Uuid::new_v4()))
}

fn write_and_sync_temp_file(temp_path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    // Reuse backup's owner-only file creator so Unix 0600 and Windows protected
    // DACLs are applied before any secret bytes touch disk.
    let mut file = crate::backup::create_private_file(temp_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(windows)]
fn replace_file_atomically(temp_path: &Path, target: &Path, _parent: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows::{
        Win32::Storage::FileSystem::{
            MOVE_FILE_FLAGS, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        },
        core::PCWSTR,
    };

    let source: Vec<u16> = temp_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let destination: Vec<u16> = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: Both buffers are NUL-terminated UTF-16 paths that stay live across the call.
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVE_FILE_FLAGS(MOVEFILE_REPLACE_EXISTING.0 | MOVEFILE_WRITE_THROUGH.0),
        )
    }
    .map_err(std::io::Error::other)
}

#[cfg(not(windows))]
fn replace_file_atomically(temp_path: &Path, target: &Path, parent: &Path) -> std::io::Result<()> {
    std::fs::rename(temp_path, target)?;
    File::open(parent)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let path = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn validates_secret_key_bounds_and_character_set() {
        assert!(validate_secret_key("CAMERA_PASS_1").is_ok());
        assert!(validate_secret_key("_LEADING_UNDERSCORE").is_ok());
        assert!(validate_secret_key(&"A".repeat(SECRET_KEY_LEN_MAX)).is_ok());

        for invalid in [
            "",
            "lowercase",
            "1STARTS_WITH_DIGIT",
            "HAS-DASH",
            "HAS SPACE",
            "HAS.DOT",
            &"A".repeat(SECRET_KEY_LEN_MAX + 1),
        ] {
            assert!(
                validate_secret_key(invalid).is_err(),
                "expected invalid key to fail: {invalid}"
            );
        }
    }

    #[test]
    fn validates_secret_value_bounds_and_rejects_nul() {
        assert!(validate_secret_value("s3cr3t!").is_ok());
        assert!(validate_secret_value(&"x".repeat(SECRET_VALUE_BYTES_MAX)).is_ok());

        assert!(validate_secret_value("").is_err());
        assert!(validate_secret_value("abc\0def").is_err());
        assert!(validate_secret_value(&"x".repeat(SECRET_VALUE_BYTES_MAX + 1)).is_err());
    }

    #[test]
    fn set_list_and_remove_persist_atomically_with_private_permissions() {
        let dir = TempDir::new("keeppeek-secrets-store");
        let path = dir.0.join("secrets.toml");
        let sentinel = "SENTINEL_SECRET_VALUE_987654321";

        assert!(list_secret_keys(&path).unwrap().is_empty());
        assert!(!remove_secret_entry(&path, "MISSING_KEY").unwrap());

        set_secret_entry(&path, "Z_CAM_PASS", sentinel).unwrap();
        set_secret_entry(&path, "A_CAM_USER", "admin-user").unwrap();

        let keys = list_secret_keys(&path).unwrap();
        assert_eq!(keys, vec!["A_CAM_USER".to_owned(), "Z_CAM_PASS".to_owned()]);
        assert!(!keys.iter().any(|entry| entry.contains(sentinel)));

        let loaded = load_secrets_file(&path).unwrap();
        assert_eq!(
            resolve_loaded_reference("{secret:Z_CAM_PASS}", &loaded)
                .unwrap()
                .as_str(),
            sentinel
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        #[cfg(windows)]
        {
            let file = File::open(&path).unwrap();
            crate::backup::assert_private(&file);
        }

        // Update an existing key and confirm no stale temporary files remain.
        set_secret_entry(&path, "Z_CAM_PASS", "rotated-value").unwrap();
        let updated = load_secrets_file(&path).unwrap();
        assert_eq!(
            resolve_loaded_reference("{secret:Z_CAM_PASS}", &updated)
                .unwrap()
                .as_str(),
            "rotated-value"
        );
        let dir_entries: Vec<_> = std::fs::read_dir(&dir.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(dir_entries, vec![std::ffi::OsString::from("secrets.toml")]);

        assert!(remove_secret_entry(&path, "Z_CAM_PASS").unwrap());
        assert!(!remove_secret_entry(&path, "Z_CAM_PASS").unwrap());
        assert_eq!(list_secret_keys(&path).unwrap(), vec!["A_CAM_USER"]);
    }

    #[test]
    fn rejects_invalid_keys_without_creating_or_modifying_file() {
        let dir = TempDir::new("keeppeek-secrets-invalid");
        let path = dir.0.join("secrets.toml");

        assert!(set_secret_entry(&path, "bad-key", "value").is_err());
        assert!(!path.exists());

        set_secret_entry(&path, "GOOD_KEY", "good-value").unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(set_secret_entry(&path, "GOOD_KEY", "").is_err());
        assert!(set_secret_entry(&path, "GOOD_KEY", "a\0b").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn fingerprint_detects_creation_and_modification() {
        let dir = TempDir::new("keeppeek-secrets-fp");
        let path = dir.0.join("secrets.toml");

        assert_eq!(secrets_file_fingerprint(&path).unwrap(), None);
        set_secret_entry(&path, "FIRST_KEY", "short").unwrap();
        let first = secrets_file_fingerprint(&path).unwrap();
        assert!(first.is_some());

        set_secret_entry(&path, "SECOND_KEY", "a-much-longer-secret-value").unwrap();
        let second = secrets_file_fingerprint(&path).unwrap();
        assert_ne!(first, second);
    }
}
