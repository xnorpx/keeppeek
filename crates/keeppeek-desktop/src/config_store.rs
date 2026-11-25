//! Persistence of desktop client settings in the canonical `config.toml`.
//!
//! KeepPeek Desktop does not create a separate settings file. It reads and
//! writes `[desktop].server_url` inside the existing KeepPeek `config.toml`,
//! preserving all unrelated sections and secret references.

use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::server_url::{ServerOrigin, ServerOriginError};

/// Environment variable that overrides the configured KeepPeek server URL.
pub const SERVER_URL_ENV: &str = "KEEPPEEK_DESKTOP_SERVER_URL";

const APP_NAME: &str = "keeppeek";
const DEFAULT_CONFIG_NAME: &str = "config.toml";
const DESKTOP_SECTION: &str = "desktop";
const SERVER_URL_KEY: &str = "server_url";
const MAX_CONFIG_FILE_BYTES: u64 = 2 * 1024 * 1024;
const DEFAULT_SERVER_PORT: u16 = 8081;
const MAX_TEMP_FILE_ATTEMPTS: usize = 64;

static SAVE_LOCK: Mutex<()> = Mutex::new(());
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Identifies where the active desktop server URL came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetSource {
    /// Supplied on the command line with `--server-url`.
    CliArgument,
    /// Supplied through `KEEPPEEK_DESKTOP_SERVER_URL`.
    Environment,
    /// Loaded from `[desktop].server_url` in `config.toml`.
    ConfigDesktop,
    /// Derived from root `host` and `port` in `config.toml`.
    ConfigServerBind,
    /// Fallback default (`http://127.0.0.1:8081`).
    DefaultLocal,
}

/// Resolved server target and metadata loaded for startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTarget {
    /// Server origin selected according to precedence rules.
    pub origin: ServerOrigin,
    /// Source that supplied `origin`.
    pub source: TargetSource,
    /// Path to the active `config.toml` file.
    pub config_path: PathBuf,
    /// Value currently persisted under `[desktop].server_url`, if any.
    pub saved_server_url: Option<ServerOrigin>,
}

/// Errors produced when reading or updating `[desktop]` in `config.toml`.
#[derive(Debug)]
pub enum ConfigStoreError {
    /// Reading or writing the file failed.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The file exceeded `MAX_CONFIG_FILE_BYTES`.
    FileTooLarge { path: PathBuf, size_bytes: u64 },
    /// The configuration file is not valid TOML.
    ParseToml { path: PathBuf, detail: String },
    /// The `[desktop]` entry is not a TOML table or has an invalid field type.
    InvalidDesktopSection(String),
    /// A configured server URL failed origin validation.
    InvalidServerUrl(ServerOriginError),
    /// The file changed concurrently or did not contain the saved origin after writing.
    VerificationMismatch { path: PathBuf },
}

impl fmt::Display for ConfigStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(formatter, "Cannot access {}: {source}.", path.display())
            }
            Self::FileTooLarge { path, size_bytes } => write!(
                formatter,
                "Configuration file {} is {size_bytes} bytes, exceeding the {MAX_CONFIG_FILE_BYTES}-byte limit.",
                path.display()
            ),
            Self::ParseToml { path, detail } => {
                write!(formatter, "Cannot parse {}: {detail}.", path.display())
            }
            Self::InvalidDesktopSection(detail) => {
                write!(formatter, "Invalid [desktop] configuration: {detail}.")
            }
            Self::InvalidServerUrl(error) => error.fmt(formatter),
            Self::VerificationMismatch { path } => write!(
                formatter,
                "Configuration file {} changed while saving desktop settings. Retry the operation.",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ConfigStoreError {}

/// Returns the default OS-specific path to `config.toml`.
#[must_use]
pub fn default_config_path() -> PathBuf {
    default_config_dir().join(DEFAULT_CONFIG_NAME)
}

fn default_config_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA").filter(|value| !value.is_empty()) {
            return PathBuf::from(appdata).join(APP_NAME);
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
            return PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join(APP_NAME);
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
            return PathBuf::from(xdg).join(APP_NAME);
        }
        if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
            return PathBuf::from(home).join(".config").join(APP_NAME);
        }
    }

    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Resolves the target server origin using CLI, environment, and `config.toml`.
///
/// Precedence is:
/// 1. `cli_override` (`--server-url`)
/// 2. `env_override` (`KEEPPEEK_DESKTOP_SERVER_URL`)
/// 3. `[desktop].server_url` in `config_path`
/// 4. Root `host` and `port` in `config_path`
/// 5. `http://127.0.0.1:8081`
///
/// # Errors
///
/// Returns a [`ConfigStoreError`] when an explicit override is invalid or when
/// an existing `config.toml` cannot be read or contains an invalid `[desktop]`
/// section.
pub fn resolve_target(
    config_path: &Path,
    cli_override: Option<&str>,
    env_override: Option<&str>,
) -> Result<ResolvedTarget, ConfigStoreError> {
    let table = read_existing_table(config_path)?;
    let saved_server_url = extract_desktop_server_url(&table)?;

    if let Some(raw) = cli_override {
        let origin =
            ServerOrigin::parse_user_input(raw).map_err(ConfigStoreError::InvalidServerUrl)?;
        return Ok(ResolvedTarget {
            origin,
            source: TargetSource::CliArgument,
            config_path: config_path.to_path_buf(),
            saved_server_url,
        });
    }

    if let Some(raw) = env_override.filter(|value| !value.trim().is_empty()) {
        let origin =
            ServerOrigin::parse_user_input(raw).map_err(ConfigStoreError::InvalidServerUrl)?;
        return Ok(ResolvedTarget {
            origin,
            source: TargetSource::Environment,
            config_path: config_path.to_path_buf(),
            saved_server_url,
        });
    }

    if let Some(origin) = saved_server_url.clone() {
        return Ok(ResolvedTarget {
            origin,
            source: TargetSource::ConfigDesktop,
            config_path: config_path.to_path_buf(),
            saved_server_url,
        });
    }

    if let Some(origin) = extract_server_bind_origin(&table) {
        return Ok(ResolvedTarget {
            origin,
            source: TargetSource::ConfigServerBind,
            config_path: config_path.to_path_buf(),
            saved_server_url,
        });
    }

    Ok(ResolvedTarget {
        origin: ServerOrigin::default_local(),
        source: TargetSource::DefaultLocal,
        config_path: config_path.to_path_buf(),
        saved_server_url,
    })
}

/// Saves `origin` under `[desktop].server_url` in `config_path`.
///
/// Unrelated TOML sections, camera entries, and `{secret:...}` references are
/// preserved. The file is replaced atomically and re-read to confirm integrity.
///
/// # Errors
///
/// Returns a [`ConfigStoreError`] when reading, serializing, or atomically
/// writing `config_path` fails.
pub fn save_server_url(config_path: &Path, origin: &ServerOrigin) -> Result<(), ConfigStoreError> {
    let _guard = SAVE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut root = read_existing_table(config_path)?;
    let desktop_value = root
        .entry(DESKTOP_SECTION.to_owned())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let Some(desktop_table) = desktop_value.as_table_mut() else {
        return Err(ConfigStoreError::InvalidDesktopSection(
            "expected [desktop] to be a TOML table".to_owned(),
        ));
    };
    desktop_table.insert(
        SERVER_URL_KEY.to_owned(),
        toml::Value::String(origin.as_str().to_owned()),
    );

    let serialized =
        toml::to_string_pretty(&root).map_err(|error| ConfigStoreError::ParseToml {
            path: config_path.to_path_buf(),
            detail: error.to_string(),
        })?;
    write_private_atomically(config_path, serialized.as_bytes())?;

    let verified = read_existing_table(config_path)?;
    let persisted = extract_desktop_server_url(&verified)?;
    if persisted.as_ref() != Some(origin) {
        return Err(ConfigStoreError::VerificationMismatch {
            path: config_path.to_path_buf(),
        });
    }
    Ok(())
}

fn read_existing_table(config_path: &Path) -> Result<toml::Table, ConfigStoreError> {
    let metadata = match fs::metadata(config_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(toml::Table::new()),
        Err(source) => {
            return Err(ConfigStoreError::Io {
                path: config_path.to_path_buf(),
                source,
            });
        }
    };
    if metadata.len() > MAX_CONFIG_FILE_BYTES {
        return Err(ConfigStoreError::FileTooLarge {
            path: config_path.to_path_buf(),
            size_bytes: metadata.len(),
        });
    }
    let text = fs::read_to_string(config_path).map_err(|source| ConfigStoreError::Io {
        path: config_path.to_path_buf(),
        source,
    })?;
    toml::from_str(&text).map_err(|error| ConfigStoreError::ParseToml {
        path: config_path.to_path_buf(),
        detail: error.to_string(),
    })
}

fn extract_desktop_server_url(
    table: &toml::Table,
) -> Result<Option<ServerOrigin>, ConfigStoreError> {
    let Some(section) = table.get(DESKTOP_SECTION) else {
        return Ok(None);
    };
    let Some(desktop) = section.as_table() else {
        return Err(ConfigStoreError::InvalidDesktopSection(
            "expected [desktop] to be a TOML table".to_owned(),
        ));
    };
    for key in desktop.keys() {
        if key != SERVER_URL_KEY {
            return Err(ConfigStoreError::InvalidDesktopSection(format!(
                "unknown field `{key}`"
            )));
        }
    }
    let Some(raw_value) = desktop.get(SERVER_URL_KEY) else {
        return Ok(None);
    };
    let Some(raw) = raw_value.as_str() else {
        return Err(ConfigStoreError::InvalidDesktopSection(
            "server_url must be a string".to_owned(),
        ));
    };
    let origin = ServerOrigin::parse_canonical(raw).map_err(ConfigStoreError::InvalidServerUrl)?;
    Ok(Some(origin))
}

fn extract_server_bind_origin(table: &toml::Table) -> Option<ServerOrigin> {
    let has_host = table.contains_key("host");
    let has_port = table.contains_key("port");
    if !has_host && !has_port {
        return None;
    }
    let host = table
        .get("host")
        .and_then(toml::Value::as_str)
        .unwrap_or("127.0.0.1");
    let port = table
        .get("port")
        .and_then(toml::Value::as_integer)
        .and_then(|value| u16::try_from(value).ok())
        .unwrap_or(DEFAULT_SERVER_PORT);
    ServerOrigin::from_server_bind(host, port).ok()
}

fn write_private_atomically(path: &Path, bytes: &[u8]) -> Result<(), ConfigStoreError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let created_parent = !parent.exists();
    fs::create_dir_all(parent).map_err(|source| ConfigStoreError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    if created_parent {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(|source| {
            ConfigStoreError::Io {
                path: parent.to_path_buf(),
                source,
            }
        })?;
    }
    #[cfg(not(unix))]
    let _ = created_parent;

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(DEFAULT_CONFIG_NAME);
    let temp_path = create_unique_temp_file(parent, file_name, bytes)?;
    if let Err(source) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(ConfigStoreError::Io {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(())
}

fn create_unique_temp_file(
    parent: &Path,
    file_name: &str,
    bytes: &[u8],
) -> Result<PathBuf, ConfigStoreError> {
    let timestamp_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0_u128, |duration| duration.as_nanos());
    for _ in 0..MAX_TEMP_FILE_ATTEMPTS {
        let sequence = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{file_name}.desktop-{}-{timestamp_nanos}-{sequence}.tmp",
            std::process::id()
        ));
        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = match options.open(&candidate) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(ConfigStoreError::Io {
                    path: candidate,
                    source,
                });
            }
        };
        if let Err(error) = write_and_sync(&mut file, &candidate, bytes) {
            let _ = fs::remove_file(&candidate);
            return Err(error);
        }
        return Ok(candidate);
    }
    Err(ConfigStoreError::Io {
        path: parent.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "unable to allocate unique temporary configuration file",
        ),
    })
}

fn write_and_sync(file: &mut fs::File, path: &Path, bytes: &[u8]) -> Result<(), ConfigStoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| ConfigStoreError::Io {
                path: path.to_path_buf(),
                source,
            })?;
    }
    file.write_all(bytes)
        .map_err(|source| ConfigStoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    file.sync_all().map_err(|source| ConfigStoreError::Io {
        path: path.to_path_buf(),
        source,
    })
}
