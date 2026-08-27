pub use crate::access::AccessKey;
use crate::{access, cameras::CameraConfig};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
};
use url::Url;

const DEFAULT_CONFIG_NAME: &str = "config.toml";
const DEFAULT_SECRETS_NAME: &str = "secrets.toml";
const ACCESS_KEY_SECRET: &str = "KEEPPEEK_ACCESS_KEY";
const STORAGE_MIGRATION_SECTION: &str = "storage_migration";
const DEFAULT_SECRETS_TEMPLATE: &str = r#"# Keep this file private. KeepPeek creates it with owner-only permissions.
# It is a flat string-to-string map. Reference values from config.toml with
# {secret:KEY}; use {secret:KEY|url} for percent-encoded URL components.

# CAMERA_USERNAME = "admin"
# CAMERA_PASSWORD = "replace-me"
# FRONT_CAMERA_PASSWORD = "replace-me"
# KEEPPEEK_ACCESS_KEY = "replace-with-a-UUID"
# HOME_ASSISTANT_TOKEN = "replace-me"
"#;

/// Flat private values loaded from `secrets.toml` beside the application configuration.
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(transparent)]
pub struct Secrets(BTreeMap<String, String>);

impl Secrets {
    pub(crate) fn redaction_values(&self) -> Vec<String> {
        self.0
            .iter()
            .filter_map(|(key, value)| {
                environment_secret(key)
                    .ok()
                    .flatten()
                    .or_else(|| Some(value.clone()))
            })
            .filter(|value| !value.is_empty())
            .collect()
    }
}

/// Shared camera fields from `[camera_defaults]` in `config.toml`.
#[derive(Clone, Default, Deserialize)]
pub struct CameraCredentialDefaults {
    /// Default camera login username, which may contain a secret reference.
    #[serde(default)]
    pub username: String,
    /// Default camera login password, which may contain a secret reference.
    #[serde(default)]
    pub password: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Config {
    #[serde(default = "default_host")]
    pub host: String,

    #[serde(default = "default_port")]
    pub port: u16,

    #[serde(default)]
    pub access_key: AccessKey,

    #[serde(default)]
    pub access: AccessConfig,

    #[serde(default)]
    pub direct_card: DirectCardConfig,

    #[serde(default)]
    pub storage: StorageToml,

    #[serde(default)]
    pub battery_wake: BatteryWakeConfig,

    #[serde(default)]
    pub logging: LoggingConfig,

    #[serde(skip)]
    pub(crate) source: toml::Table,
}

impl Config {
    /// Returns a raw secret reference for a string field, or its resolved value.
    pub fn reference_or_value(&self, path: &[&str], resolved: &str) -> String {
        let mut value = path.first().and_then(|segment| self.source.get(*segment));
        for segment in path.iter().skip(1) {
            value = value
                .and_then(toml::Value::as_table)
                .and_then(|table| table.get(*segment));
        }
        value
            .and_then(toml::Value::as_str)
            .filter(|value| contains_secret_reference(value))
            .unwrap_or(resolved)
            .to_owned()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AccessConfig {
    #[serde(default = "access::default_local_networks")]
    pub local_networks: Vec<IpNet>,
    #[serde(default)]
    pub trusted_proxies: Vec<IpNet>,
    #[serde(default = "default_require_secure_remote")]
    pub require_secure_remote: bool,
    #[serde(default = "default_failed_authentication_limit")]
    pub failed_authentication_limit: u32,
    #[serde(default = "default_failed_authentication_window_secs")]
    pub failed_authentication_window_secs: u64,
    #[serde(default = "default_session_idle_timeout_secs")]
    pub session_idle_timeout_secs: u64,
    #[serde(default = "default_session_absolute_timeout_secs")]
    pub session_absolute_timeout_secs: u64,
    #[serde(default = "default_max_sessions_per_principal")]
    pub max_sessions_per_principal: u32,
    #[serde(default = "default_max_sessions_per_address")]
    pub max_sessions_per_address: u32,
}

impl AccessConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.local_networks.len() > 64 || self.trusted_proxies.len() > 64 {
            anyhow::bail!("access network lists may contain at most 64 CIDRs each");
        }
        if self.failed_authentication_limit == 0 || self.failed_authentication_window_secs == 0 {
            anyhow::bail!("failed authentication rate limits must be nonzero");
        }
        if self.session_idle_timeout_secs == 0 || self.session_absolute_timeout_secs == 0 {
            anyhow::bail!("access session timeouts must be nonzero");
        }
        if self.session_idle_timeout_secs > self.session_absolute_timeout_secs {
            anyhow::bail!("access session idle timeout cannot exceed its absolute timeout");
        }
        if self.max_sessions_per_principal == 0 || self.max_sessions_per_address == 0 {
            anyhow::bail!("access session limits must be nonzero");
        }
        Ok(())
    }
}

impl Default for AccessConfig {
    fn default() -> Self {
        Self {
            local_networks: access::default_local_networks(),
            trusted_proxies: Vec::new(),
            require_secure_remote: default_require_secure_remote(),
            failed_authentication_limit: default_failed_authentication_limit(),
            failed_authentication_window_secs: default_failed_authentication_window_secs(),
            session_idle_timeout_secs: default_session_idle_timeout_secs(),
            session_absolute_timeout_secs: default_session_absolute_timeout_secs(),
            max_sessions_per_principal: default_max_sessions_per_principal(),
            max_sessions_per_address: default_max_sessions_per_address(),
        }
    }
}

const fn default_require_secure_remote() -> bool {
    true
}

const fn default_failed_authentication_limit() -> u32 {
    5
}

const fn default_failed_authentication_window_secs() -> u64 {
    60
}

const fn default_session_idle_timeout_secs() -> u64 {
    30 * 60
}

const fn default_session_absolute_timeout_secs() -> u64 {
    24 * 60 * 60
}

const fn default_max_sessions_per_principal() -> u32 {
    64
}

const fn default_max_sessions_per_address() -> u32 {
    128
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct DirectCardConfig {
    #[serde(default)]
    pub allowed_origins: Vec<String>,
}

impl DirectCardConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        let mut unique = HashSet::with_capacity(self.allowed_origins.len());
        for origin in &self.allowed_origins {
            let url = Url::parse(origin)
                .map_err(|error| anyhow::anyhow!("invalid direct card origin: {error}"))?;
            if !matches!(url.scheme(), "http" | "https")
                || url.path() != "/"
                || url.query().is_some()
                || url.fragment().is_some()
                || !url.username().is_empty()
                || url.password().is_some()
            {
                anyhow::bail!(
                    "direct card origin must contain only an HTTP(S) scheme and authority"
                );
            }
            let canonical = url.origin().ascii_serialization();
            if canonical == "null" || canonical != *origin {
                anyhow::bail!(
                    "direct card origin must use its canonical exact origin: {canonical}"
                );
            }
            if !unique.insert(origin) {
                anyhow::bail!("direct card origin appears more than once: {origin}");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceLogDestination {
    EventLog,
    #[default]
    File,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct LoggingConfig {
    #[serde(default)]
    pub service: ServiceLogDestination,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BatteryWakeConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub bind: Option<Ipv4Addr>,

    #[serde(default = "default_battery_wake_middleman_port")]
    pub middleman_port: u16,

    #[serde(default = "default_battery_wake_register_port")]
    pub register_port: u16,

    #[serde(default = "default_battery_wake_heartbeat_secs")]
    pub heartbeat_secs: u64,

    #[serde(default = "default_battery_wake_stale_after_secs")]
    pub stale_after_secs: u64,
}

impl BatteryWakeConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.middleman_port == 0 || self.register_port == 0 {
            anyhow::bail!("battery wake ports must be non-zero");
        }
        if self.middleman_port == self.register_port {
            anyhow::bail!("battery wake middleman and register ports must differ");
        }
        if self.heartbeat_secs == 0 {
            anyhow::bail!("battery wake heartbeat interval must be non-zero");
        }
        if self.stale_after_secs < self.heartbeat_secs {
            anyhow::bail!("battery wake stale timeout must cover one heartbeat interval");
        }
        Ok(())
    }
}

impl Default for BatteryWakeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: None,
            middleman_port: default_battery_wake_middleman_port(),
            register_port: default_battery_wake_register_port(),
            heartbeat_secs: default_battery_wake_heartbeat_secs(),
            stale_after_secs: default_battery_wake_stale_after_secs(),
        }
    }
}

const fn default_battery_wake_middleman_port() -> u16 {
    9_999
}

const fn default_battery_wake_register_port() -> u16 {
    58_200
}

const fn default_battery_wake_heartbeat_secs() -> u64 {
    20
}

const fn default_battery_wake_stale_after_secs() -> u64 {
    80
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StorageToml {
    #[serde(default)]
    pub medium_term_path: Option<String>,

    #[serde(default)]
    pub long_term_path: Option<String>,

    #[serde(default)]
    pub recording_catalog_path: Option<String>,

    #[serde(default)]
    pub event_thumbnail_path: Option<String>,

    #[serde(default = "default_event_thumbnail_max_mb")]
    pub event_thumbnail_max_mb: u64,

    #[serde(default = "default_short_term_secs")]
    pub short_term_secs: u64,

    #[serde(default = "default_medium_term_secs")]
    pub medium_term_secs: u64,

    #[serde(default = "default_flush_interval_secs")]
    pub flush_interval_secs: u64,

    #[serde(default = "default_write_buffer_bytes")]
    pub write_buffer_bytes: usize,

    #[serde(default = "default_long_term_max_gb")]
    pub long_term_max_gb: u64,

    #[serde(default = "default_minimum_free_gb")]
    pub minimum_free_gb: u64,

    #[serde(default)]
    pub maximum_used_percent: Option<u8>,

    #[serde(default = "default_warning_free_gb")]
    pub warning_free_gb: u64,

    #[serde(default = "default_critical_free_gb")]
    pub critical_free_gb: u64,

    #[serde(default = "default_cleanup_hysteresis_gb")]
    pub cleanup_hysteresis_gb: u64,
}

impl StorageToml {
    pub(crate) fn validate_safety_thresholds(&self) -> anyhow::Result<()> {
        if self
            .maximum_used_percent
            .is_some_and(|percent| !(1..=99).contains(&percent))
        {
            anyhow::bail!("maximum filesystem usage must be between 1 and 99 percent");
        }
        let critical_free_gb = self.critical_free_gb.max(self.minimum_free_gb);
        let warning_free_gb = if self.warning_free_gb == 0 && critical_free_gb > 0 {
            critical_free_gb.saturating_add(self.cleanup_hysteresis_gb)
        } else {
            self.warning_free_gb
        };
        if warning_free_gb < critical_free_gb {
            anyhow::bail!(
                "warning free space must be greater than or equal to critical free space"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageMigration {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub medium_term: Option<StoragePathMigration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub long_term: Option<StoragePathMigration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording_catalog: Option<StoragePathMigration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_thumbnails: Option<StoragePathMigration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_catalog_after_move: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoragePathMigration {
    pub from: PathBuf,
    pub to: PathBuf,
}

pub struct StorageMigrationPaths<'a> {
    medium_term_path: &'a Path,
    long_term_path: &'a Path,
    recording_catalog_path: &'a Path,
    event_thumbnail_path: &'a Path,
}

impl<'a> StorageMigrationPaths<'a> {
    pub const fn new(
        medium_term_path: &'a Path,
        long_term_path: &'a Path,
        recording_catalog_path: &'a Path,
        event_thumbnail_path: &'a Path,
    ) -> Self {
        Self {
            medium_term_path,
            long_term_path,
            recording_catalog_path,
            event_thumbnail_path,
        }
    }
}

impl StorageMigration {
    pub fn between(
        current_medium_term_path: &Path,
        next_medium_term_path: &Path,
        current_long_term_path: &Path,
        next_long_term_path: &Path,
    ) -> anyhow::Result<Option<Self>> {
        Self::between_paths(
            current_medium_term_path,
            next_medium_term_path,
            current_long_term_path,
            next_long_term_path,
            None,
        )
    }

    pub fn between_with_metadata(
        current: StorageMigrationPaths<'_>,
        next: StorageMigrationPaths<'_>,
    ) -> anyhow::Result<Option<Self>> {
        Self::between_paths(
            current.medium_term_path,
            next.medium_term_path,
            current.long_term_path,
            next.long_term_path,
            Some((
                current.recording_catalog_path,
                next.recording_catalog_path,
                current.event_thumbnail_path,
                next.event_thumbnail_path,
            )),
        )
    }

    fn between_paths(
        current_medium_term_path: &Path,
        next_medium_term_path: &Path,
        current_long_term_path: &Path,
        next_long_term_path: &Path,
        metadata_paths: Option<(&Path, &Path, &Path, &Path)>,
    ) -> anyhow::Result<Option<Self>> {
        let mut migration = Self {
            medium_term: (current_medium_term_path != next_medium_term_path).then(|| {
                StoragePathMigration {
                    from: current_medium_term_path.to_path_buf(),
                    to: next_medium_term_path.to_path_buf(),
                }
            }),
            long_term: (current_long_term_path != next_long_term_path).then(|| {
                StoragePathMigration {
                    from: current_long_term_path.to_path_buf(),
                    to: next_long_term_path.to_path_buf(),
                }
            }),
            recording_catalog: None,
            event_thumbnails: None,
            recording_catalog_after_move: None,
        };
        if let Some((
            current_recording_catalog_path,
            next_recording_catalog_path,
            current_event_thumbnail_path,
            next_event_thumbnail_path,
        )) = metadata_paths
        {
            migration.recording_catalog_after_move =
                Some(next_recording_catalog_path.to_path_buf());
            if current_recording_catalog_path != next_recording_catalog_path
                && !migration.is_covered_by_recording_root(
                    current_recording_catalog_path,
                    next_recording_catalog_path,
                )
            {
                migration.recording_catalog = Some(StoragePathMigration {
                    from: current_recording_catalog_path.to_path_buf(),
                    to: next_recording_catalog_path.to_path_buf(),
                });
            }
            if current_event_thumbnail_path != next_event_thumbnail_path
                && !migration.is_covered_by_recording_root(
                    current_event_thumbnail_path,
                    next_event_thumbnail_path,
                )
            {
                migration.event_thumbnails = Some(StoragePathMigration {
                    from: current_event_thumbnail_path.to_path_buf(),
                    to: next_event_thumbnail_path.to_path_buf(),
                });
            }
        }
        migration.validate()?;
        Ok((!migration.is_empty()).then_some(migration))
    }

    const fn is_empty(&self) -> bool {
        self.medium_term.is_none()
            && self.long_term.is_none()
            && self.recording_catalog.is_none()
            && self.event_thumbnails.is_none()
    }

    fn is_covered_by_recording_root(&self, from: &Path, to: &Path) -> bool {
        [self.medium_term.as_ref(), self.long_term.as_ref()]
            .into_iter()
            .flatten()
            .any(|route| {
                matches!(
                    (from.strip_prefix(&route.from), to.strip_prefix(&route.to)),
                    (Ok(from_relative), Ok(to_relative)) if from_relative == to_relative
                )
            })
    }

    fn routes(&self) -> impl Iterator<Item = &StoragePathMigration> {
        [
            self.recording_catalog.as_ref(),
            self.event_thumbnails.as_ref(),
            self.medium_term.as_ref(),
            self.long_term.as_ref(),
        ]
        .into_iter()
        .flatten()
    }

    fn validate(&self) -> anyhow::Result<()> {
        let routes = self.routes().collect::<Vec<_>>();
        for route in &routes {
            if route.from.as_os_str().is_empty() || route.to.as_os_str().is_empty() {
                anyhow::bail!("storage migration paths must not be empty");
            }
            if route.to.starts_with(&route.from) || route.from.starts_with(&route.to) {
                anyhow::bail!(
                    "storage migration paths must not contain one another: {} and {}",
                    route.from.display(),
                    route.to.display()
                );
            }
        }
        if self
            .recording_catalog_after_move
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            anyhow::bail!("post-migration recording catalog path must not be empty");
        }
        for (index, route) in routes.iter().enumerate() {
            for other in routes.iter().skip(index + 1) {
                if route.from == other.from && route.to != other.to {
                    anyhow::bail!(
                        "cannot split one current storage path into two destinations while moving recordings"
                    );
                }
                if route.from == other.to || route.to == other.from {
                    anyhow::bail!("storage migration paths must not overlap");
                }
            }
        }
        Ok(())
    }

    fn apply(&self) -> anyhow::Result<()> {
        self.validate()?;
        let mut recording_routes = [self.medium_term.as_ref(), self.long_term.as_ref()]
            .into_iter()
            .flatten()
            .map(|route| (route.from.clone(), route.to.clone()))
            .collect::<Vec<_>>();
        recording_routes.sort_unstable();
        recording_routes.dedup();
        let mut moved = Vec::new();
        for route in self.routes() {
            if moved
                .iter()
                .any(|(from, to): &(PathBuf, PathBuf)| from == &route.from && to == &route.to)
            {
                continue;
            }
            if self.recording_catalog.as_ref() == Some(route) {
                move_recording_catalog_path(&route.from, &route.to)?;
            } else {
                move_storage_path(&route.from, &route.to)?;
            }
            moved.push((route.from.clone(), route.to.clone()));
        }
        if let Some(catalog_path) = &self.recording_catalog_after_move {
            crate::storage::catalog::rewrite_recording_paths(catalog_path, &recording_routes)?;
        }
        Ok(())
    }
}

const fn default_short_term_secs() -> u64 {
    120
}

const fn default_medium_term_secs() -> u64 {
    1800
}

const fn default_flush_interval_secs() -> u64 {
    60
}

const fn default_write_buffer_bytes() -> usize {
    8 * 1024
}

const fn default_event_thumbnail_max_mb() -> u64 {
    1_024
}

const fn default_long_term_max_gb() -> u64 {
    1_024
}

const fn default_minimum_free_gb() -> u64 {
    10
}

const fn default_warning_free_gb() -> u64 {
    20
}

const fn default_critical_free_gb() -> u64 {
    10
}

const fn default_cleanup_hysteresis_gb() -> u64 {
    5
}

impl Default for StorageToml {
    fn default() -> Self {
        Self {
            medium_term_path: None,
            long_term_path: None,
            recording_catalog_path: None,
            event_thumbnail_path: None,
            event_thumbnail_max_mb: default_event_thumbnail_max_mb(),
            short_term_secs: default_short_term_secs(),
            medium_term_secs: default_medium_term_secs(),
            flush_interval_secs: default_flush_interval_secs(),
            write_buffer_bytes: default_write_buffer_bytes(),
            long_term_max_gb: default_long_term_max_gb(),
            minimum_free_gb: default_minimum_free_gb(),
            maximum_used_percent: None,
            warning_free_gb: default_warning_free_gb(),
            critical_free_gb: default_critical_free_gb(),
            cleanup_hysteresis_gb: default_cleanup_hysteresis_gb(),
        }
    }
}

fn default_host() -> String {
    "0.0.0.0".to_owned()
}

const fn default_port() -> u16 {
    8081
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            access_key: AccessKey::unset(),
            access: AccessConfig::default(),
            direct_card: DirectCardConfig::default(),
            storage: StorageToml::default(),
            battery_wake: BatteryWakeConfig::default(),
            logging: LoggingConfig::default(),
            source: toml::Table::new(),
        }
    }
}

const APP_NAME: &str = "keeppeek";

pub fn config_dir() -> PathBuf {
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

    // last resort: next to the executable
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Creates and returns the OS-private KeepPeek configuration directory.
pub fn ensure_config_dir() -> std::io::Result<PathBuf> {
    let directory = config_dir();
    std::fs::create_dir_all(&directory)?;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(directory)
}

/// Writes a file containing private application data with owner-only permissions on Unix.
pub fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(bytes)
}

/// Returns the canonical KeepPeek configuration file path.
pub fn config_path() -> PathBuf {
    config_dir().join(DEFAULT_CONFIG_NAME)
}

/// Returns the secrets file stored beside a KeepPeek configuration.
pub fn secrets_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(DEFAULT_SECRETS_NAME)
}

/// Loads private credentials stored beside a KeepPeek configuration.
pub fn load_secrets(config_path: &Path) -> anyhow::Result<Secrets> {
    let path = secrets_path(config_path);
    if !path.exists() {
        return Ok(Secrets::default());
    }
    #[cfg(unix)]
    make_file_owner_only(&path)?;
    let text = std::fs::read_to_string(&path)?;
    let secrets = toml::from_str(&text).map_err(|_| {
        anyhow::anyhow!(
            "unable to parse {}; secrets must be a flat string-to-string TOML table",
            path.display()
        )
    })?;
    validate_secret_keys(&secrets)?;
    Ok(secrets)
}

#[cfg(unix)]
fn make_file_owner_only(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

fn ensure_secrets_file(config_path: &Path) -> anyhow::Result<Secrets> {
    let path = secrets_path(config_path);
    if path.exists() {
        return load_secrets(config_path);
    }
    write_private_file(&path, DEFAULT_SECRETS_TEMPLATE.as_bytes())?;
    Ok(Secrets::default())
}

fn ensure_access_key_secret(
    config_path: &Path,
    secrets: &mut Secrets,
) -> anyhow::Result<AccessKey> {
    let existing = environment_secret(ACCESS_KEY_SECRET)?
        .or_else(|| secrets.0.get(ACCESS_KEY_SECRET).cloned());
    if let Some(existing) = existing {
        return AccessKey::parse(&existing)
            .map_err(|error| anyhow::anyhow!("invalid {ACCESS_KEY_SECRET} secret: {error}"));
    }

    let access_key = AccessKey::generate();
    secrets
        .0
        .insert(ACCESS_KEY_SECRET.to_owned(), access_key.canonical());
    let serialized = toml::to_string_pretty(secrets)?;
    write_private_file(&secrets_path(config_path), serialized.as_bytes())?;
    Ok(access_key)
}

fn store_access_key_secret(
    config_path: &Path,
    secrets: &mut Secrets,
    access_key: AccessKey,
) -> anyhow::Result<()> {
    secrets
        .0
        .insert(ACCESS_KEY_SECRET.to_owned(), access_key.canonical());
    let serialized = toml::to_string_pretty(secrets)?;
    write_private_file_atomically(&secrets_path(config_path), serialized.as_bytes())?;
    Ok(())
}

pub(crate) fn rotate_access_key_secret(config_path: &Path) -> anyhow::Result<AccessKey> {
    if environment_secret(ACCESS_KEY_SECRET)?.is_some() {
        anyhow::bail!(
            "remote access key rotation is unavailable while KEEPPEEK_SECRET_{ACCESS_KEY_SECRET} is set"
        );
    }
    let mut secrets = ensure_secrets_file(config_path)?;
    let access_key = AccessKey::generate();
    store_access_key_secret(config_path, &mut secrets, access_key)?;
    Ok(access_key)
}

fn validate_secret_keys(secrets: &Secrets) -> anyhow::Result<()> {
    for key in secrets.0.keys() {
        if !valid_secret_key(key) {
            anyhow::bail!(
                "invalid secret key '{key}'; use uppercase letters, digits, and underscores"
            );
        }
    }
    Ok(())
}

fn valid_secret_key(key: &str) -> bool {
    let mut characters = key.chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_uppercase() || character == '_')
        && characters.all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
}

/// Resolves `{secret:KEY}` and `{secret:KEY|url}` references in a string.
pub fn resolve_secret_references(config_path: &Path, value: &str) -> anyhow::Result<String> {
    let secrets = load_secrets(config_path)?;
    resolve_secret_references_loaded(value, &secrets)
}

fn resolve_secret_references_loaded(value: &str, secrets: &Secrets) -> anyhow::Result<String> {
    resolve_secret_references_with(value, secrets, environment_secret)
}

fn environment_secret(key: &str) -> anyhow::Result<Option<String>> {
    match std::env::var(format!("KEEPPEEK_SECRET_{key}")) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            anyhow::bail!("environment override for secret key '{key}' is not valid Unicode")
        }
    }
}

fn resolve_secret_references_with(
    value: &str,
    secrets: &Secrets,
    environment: impl Fn(&str) -> anyhow::Result<Option<String>>,
) -> anyhow::Result<String> {
    const PREFIX: &str = "{secret:";

    let mut resolved = String::with_capacity(value.len());
    let mut cursor = 0;
    while let Some(offset) = value[cursor..].find(PREFIX) {
        let reference_start = cursor + offset;
        resolved.push_str(&value[cursor..reference_start]);
        let body_start = reference_start + PREFIX.len();
        let Some(body_end_offset) = value[body_start..].find('}') else {
            anyhow::bail!("malformed secret reference");
        };
        let body_end = body_start + body_end_offset;
        let body = &value[body_start..body_end];
        let (key, modifier) = body
            .split_once('|')
            .map_or((body, None), |(key, modifier)| (key, Some(modifier)));
        if !valid_secret_key(key) {
            anyhow::bail!("invalid secret key '{key}'");
        }
        let secret = environment(key)?
            .or_else(|| secrets.0.get(key).cloned())
            .ok_or_else(|| anyhow::anyhow!("missing secret key '{key}'"))?;
        match modifier {
            None => resolved.push_str(&secret),
            Some("url") => resolved.push_str(&percent_encode_url_component(&secret)),
            Some(_) => anyhow::bail!("invalid modifier for secret key '{key}'"),
        }
        cursor = body_end + 1;
    }
    resolved.push_str(&value[cursor..]);
    Ok(resolved)
}

fn percent_encode_url_component(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

pub(crate) fn contains_secret_reference(value: &str) -> bool {
    value.contains("{secret:")
}

fn resolve_toml_secret_references(
    value: &mut toml::Value,
    secrets: &Secrets,
) -> anyhow::Result<()> {
    match value {
        toml::Value::String(value) => {
            *value = resolve_secret_references_loaded(value, secrets)?;
        }
        toml::Value::Array(values) => {
            for value in values {
                resolve_toml_secret_references(value, secrets)?;
            }
        }
        toml::Value::Table(values) => {
            for (_, value) in values.iter_mut() {
                resolve_toml_secret_references(value, secrets)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn read_config_arg() -> Option<PathBuf> {
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--config" | "-c" => {
                if i + 1 < args.len() {
                    return Some(PathBuf::from(&args[i + 1]));
                }
            }
            _ => {
                if let Some(val) = args[i].strip_prefix("--config=") {
                    return Some(PathBuf::from(val));
                }
            }
        }
        i += 1;
    }
    None
}

fn read_access_key_arg() -> anyhow::Result<Option<AccessKey>> {
    let args: Vec<String> = std::env::args().collect();
    let mut index = 1;
    while index < args.len() {
        if args[index] == "--access-key" {
            let value = args
                .get(index + 1)
                .ok_or_else(|| anyhow::anyhow!("--access-key requires a UUID"))?;
            return AccessKey::parse(value)
                .map(Some)
                .map_err(|error| anyhow::anyhow!("invalid --access-key: {error}"));
        }
        if let Some(value) = args[index].strip_prefix("--access-key=") {
            return AccessKey::parse(value)
                .map(Some)
                .map_err(|error| anyhow::anyhow!("invalid --access-key: {error}"));
        }
        index += 1;
    }
    Ok(None)
}

pub fn load() -> anyhow::Result<(Config, PathBuf)> {
    let path = read_config_arg().unwrap_or_else(config_path);
    let config_directory = ensure_config_dir()?;
    let mut secrets = ensure_secrets_file(&path)?;

    let (mut cfg, mut merged, existing_config) = if path.exists() {
        let text = std::fs::read_to_string(&path)?;
        let mut root: toml::Table = toml::from_str(&text)?;
        apply_pending_storage_migration(&mut root)?;
        let cfg = config_from_table(&root, &secrets)?;
        (cfg, root, true)
    } else {
        (Config::default(), toml::Table::new(), false)
    };

    let access_key_arg = read_access_key_arg()?.filter(|key| !key.is_unset());
    if let Some(access_key) = access_key_arg {
        cfg.access_key = access_key;
    }
    let access_key_is_reference = merged
        .get("access_key")
        .and_then(toml::Value::as_str)
        .is_some_and(contains_secret_reference);
    let generated_access_key = cfg.access_key.is_unset();
    if generated_access_key {
        cfg.access_key = ensure_access_key_secret(&path, &mut secrets)?;
    } else if !access_key_is_reference || access_key_arg.is_some() {
        store_access_key_secret(&path, &mut secrets, cfg.access_key)?;
    }
    cfg.access.validate()?;
    cfg.direct_card.validate()?;

    let default_recordings = config_directory
        .join("recordings")
        .to_string_lossy()
        .into_owned();
    if cfg.storage.medium_term_path.is_none() {
        cfg.storage.medium_term_path = Some(default_recordings.clone());
    }
    if cfg.storage.long_term_path.is_none() {
        cfg.storage.long_term_path = Some(default_recordings);
    }

    let cfg_value: toml::Value = toml::Value::try_from(&cfg)?;
    let cfg_table = cfg_value.as_table().cloned().unwrap_or_default();

    for (key, value) in cfg_table {
        match merged.get_mut(&key) {
            Some(existing) => merge_preserving_secret_references(existing, value),
            None => {
                merged.insert(key, value);
            }
        }
    }
    if generated_access_key || !access_key_is_reference || access_key_arg.is_some() {
        merged.insert(
            "access_key".to_owned(),
            toml::Value::String(format!("{{secret:{ACCESS_KEY_SECRET}}}")),
        );
    }

    let text = toml::to_string_pretty(&merged)?;
    write_private_file(&path, text.as_bytes())?;

    if !existing_config {
        tracing::info!("created default config at {}", path.display());
    }
    if generated_access_key {
        tracing::info!("created owner-only remote access key; the value is not logged");
    } else if !access_key_is_reference || access_key_arg.is_some() {
        tracing::info!("stored the remote access key in the owner-only secret file");
    }

    Ok((cfg, path))
}

pub fn update_settings(path: &Path, settings: &Config) -> anyhow::Result<Config> {
    update_settings_with_migration(path, settings, None)
}

pub fn update_settings_with_migration(
    path: &Path,
    settings: &Config,
    migration: Option<&StorageMigration>,
) -> anyhow::Result<Config> {
    let text = std::fs::read_to_string(path)?;
    let mut root: toml::Table = toml::from_str(&text)?;
    let secrets = load_secrets(path)?;
    set_string_preserving_secret_reference(&mut root, "host", &settings.host, &secrets)?;
    root.insert(
        "port".to_owned(),
        toml::Value::Integer(i64::from(settings.port)),
    );
    let storage = root
        .entry("storage".to_owned())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("storage is not a configuration table"))?;
    if let Some(medium_term_path) = &settings.storage.medium_term_path {
        set_string_preserving_secret_reference(
            storage,
            "medium_term_path",
            medium_term_path,
            &secrets,
        )?;
    }
    if let Some(long_term_path) = &settings.storage.long_term_path {
        set_string_preserving_secret_reference(
            storage,
            "long_term_path",
            long_term_path,
            &secrets,
        )?;
    }
    match &settings.storage.recording_catalog_path {
        Some(recording_catalog_path) => {
            set_string_preserving_secret_reference(
                storage,
                "recording_catalog_path",
                recording_catalog_path,
                &secrets,
            )?;
        }
        None => {
            storage.remove("recording_catalog_path");
        }
    }
    match &settings.storage.event_thumbnail_path {
        Some(event_thumbnail_path) => {
            set_string_preserving_secret_reference(
                storage,
                "event_thumbnail_path",
                event_thumbnail_path,
                &secrets,
            )?;
        }
        None => {
            storage.remove("event_thumbnail_path");
        }
    }
    storage.insert(
        "event_thumbnail_max_mb".to_owned(),
        toml::Value::Integer(i64::try_from(settings.storage.event_thumbnail_max_mb)?),
    );
    storage.insert(
        "short_term_secs".to_owned(),
        toml::Value::Integer(i64::try_from(settings.storage.short_term_secs)?),
    );
    storage.insert(
        "medium_term_secs".to_owned(),
        toml::Value::Integer(i64::try_from(settings.storage.medium_term_secs)?),
    );
    storage.insert(
        "flush_interval_secs".to_owned(),
        toml::Value::Integer(i64::try_from(settings.storage.flush_interval_secs)?),
    );
    storage.insert(
        "write_buffer_bytes".to_owned(),
        toml::Value::Integer(i64::try_from(settings.storage.write_buffer_bytes)?),
    );
    storage.insert(
        "long_term_max_gb".to_owned(),
        toml::Value::Integer(i64::try_from(settings.storage.long_term_max_gb)?),
    );
    storage.insert(
        "minimum_free_gb".to_owned(),
        toml::Value::Integer(i64::try_from(settings.storage.minimum_free_gb)?),
    );
    match settings.storage.maximum_used_percent {
        Some(maximum_used_percent) => {
            storage.insert(
                "maximum_used_percent".to_owned(),
                toml::Value::Integer(i64::from(maximum_used_percent)),
            );
        }
        None => {
            storage.remove("maximum_used_percent");
        }
    }
    storage.insert(
        "warning_free_gb".to_owned(),
        toml::Value::Integer(i64::try_from(settings.storage.warning_free_gb)?),
    );
    storage.insert(
        "critical_free_gb".to_owned(),
        toml::Value::Integer(i64::try_from(settings.storage.critical_free_gb)?),
    );
    storage.insert(
        "cleanup_hysteresis_gb".to_owned(),
        toml::Value::Integer(i64::try_from(settings.storage.cleanup_hysteresis_gb)?),
    );
    match migration {
        Some(migration) => {
            migration.validate()?;
            root.insert(
                STORAGE_MIGRATION_SECTION.to_owned(),
                toml::Value::try_from(migration)?,
            );
        }
        None => {
            root.remove(STORAGE_MIGRATION_SECTION);
        }
    }

    let serialized = toml::to_string_pretty(&root)?;
    let updated = config_from_table(&root, &secrets)?;
    write_private_file_atomically(path, serialized.as_bytes())?;
    Ok(updated)
}

/// Loads and resolves the application configuration without writing it.
pub fn load_config(path: &Path) -> anyhow::Result<Config> {
    let text = std::fs::read_to_string(path)?;
    let root: toml::Table = toml::from_str(&text)?;
    let secrets = load_secrets(path)?;
    config_from_table(&root, &secrets)
}

fn config_from_table(root: &toml::Table, secrets: &Secrets) -> anyhow::Result<Config> {
    let mut resolved = toml::Value::Table(root.clone());
    resolve_toml_secret_references(&mut resolved, secrets)?;
    let mut config: Config = resolved.try_into()?;
    config.source = root.clone();
    Ok(config)
}

fn merge_preserving_secret_references(existing: &mut toml::Value, next: toml::Value) {
    if contains_secret_references(existing) {
        if let (toml::Value::Table(existing), toml::Value::Table(next)) = (existing, next) {
            for (key, value) in next {
                match existing.get_mut(&key) {
                    Some(existing) => merge_preserving_secret_references(existing, value),
                    None => {
                        existing.insert(key, value);
                    }
                }
            }
        }
        return;
    }
    *existing = next;
}

fn contains_secret_references(value: &toml::Value) -> bool {
    match value {
        toml::Value::String(value) => contains_secret_reference(value),
        toml::Value::Array(values) => values.iter().any(contains_secret_references),
        toml::Value::Table(values) => values.values().any(contains_secret_references),
        _ => false,
    }
}

fn set_string_preserving_secret_reference(
    table: &mut toml::Table,
    key: &str,
    next: &str,
    secrets: &Secrets,
) -> anyhow::Result<()> {
    if let Some(existing) = table.get(key).and_then(toml::Value::as_str)
        && contains_secret_reference(existing)
        && resolve_secret_references_loaded(existing, secrets)? == next
    {
        return Ok(());
    }
    table.insert(key.to_owned(), toml::Value::String(next.to_owned()));
    Ok(())
}

pub fn load_cameras(path: &Path) -> anyhow::Result<HashMap<String, Vec<CameraConfig>>> {
    let text = std::fs::read_to_string(path)?;
    let root: toml::Value = toml::from_str(&text)?;

    let root_table = root
        .as_table()
        .ok_or_else(|| anyhow::anyhow!("cameras config is not a TOML table"))?;

    let mut result: HashMap<String, Vec<CameraConfig>> = HashMap::new();
    let secrets = load_secrets(path)?;
    let defaults = camera_defaults_from_table(root_table, &secrets)?;

    for (namespace, ns_value) in root_table {
        if is_reserved_section(namespace) {
            continue;
        }

        let Some(ns_table) = ns_value.as_table() else {
            continue;
        };

        let mut cameras = Vec::new();
        for (cam_name, cam_value) in ns_table {
            let mut resolved = cam_value.clone();
            resolve_toml_secret_references(&mut resolved, &secrets)?;
            let mut config: CameraConfig = resolved.try_into()?;
            config.name = Some(cam_name.clone());
            if config.username.is_empty() {
                config.username.clone_from(&defaults.username);
            }
            if config.password.is_empty() {
                config.password.clone_from(&defaults.password);
            }
            cameras.push(config);
        }

        result.insert(namespace.clone(), cameras);
    }

    Ok(result)
}

/// Loads resolved shared camera credentials from `[camera_defaults]` in `config.toml`.
pub fn load_camera_defaults(path: &Path) -> anyhow::Result<CameraCredentialDefaults> {
    let text = std::fs::read_to_string(path)?;
    let root: toml::Table = toml::from_str(&text)?;
    let secrets = load_secrets(path)?;
    camera_defaults_from_table(&root, &secrets)
}

fn camera_defaults_from_table(
    root: &toml::Table,
    secrets: &Secrets,
) -> anyhow::Result<CameraCredentialDefaults> {
    let Some(defaults) = root.get("camera_defaults") else {
        return Ok(CameraCredentialDefaults::default());
    };
    let mut resolved = defaults.clone();
    resolve_toml_secret_references(&mut resolved, secrets)?;
    resolved.try_into().map_err(Into::into)
}

fn is_reserved_section(namespace: &str) -> bool {
    matches!(
        namespace,
        "access"
            | "storage"
            | "battery_wake"
            | "direct_card"
            | "homekit"
            | "logging"
            | "camera_defaults"
            | STORAGE_MIGRATION_SECTION
    )
}

fn apply_pending_storage_migration(root: &mut toml::Table) -> anyhow::Result<()> {
    let Some(value) = root.get(STORAGE_MIGRATION_SECTION).cloned() else {
        return Ok(());
    };
    let migration: StorageMigration = value.try_into()?;
    migration.apply()?;
    root.remove(STORAGE_MIGRATION_SECTION);
    Ok(())
}

fn move_directory_contents(from: &Path, to: &Path) -> anyhow::Result<()> {
    if !from.exists() {
        return Ok(());
    }
    if !from.is_dir() {
        anyhow::bail!(
            "storage migration source {} is not a directory",
            from.display()
        );
    }
    std::fs::create_dir_all(to)?;
    let mut entries = std::fs::read_dir(from)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_unstable_by_key(|entry| entry.file_name());
    for entry in entries {
        let source = entry.path();
        let destination = to.join(entry.file_name());
        move_path(&source, &destination)?;
    }
    std::fs::remove_dir(from).or_else(|error| {
        (error.kind() == std::io::ErrorKind::NotFound)
            .then_some(())
            .ok_or(error)
    })?;
    Ok(())
}

fn move_storage_path(from: &Path, to: &Path) -> anyhow::Result<()> {
    if !from.exists() {
        return Ok(());
    }
    if from.is_dir() {
        return move_directory_contents(from, to);
    }
    let parent = to.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    move_path(from, to)
}

fn move_recording_catalog_path(from: &Path, to: &Path) -> anyhow::Result<()> {
    move_storage_path(from, to)?;
    for suffix in ["-wal", "-shm"] {
        let from_sidecar = path_with_suffix(from, suffix);
        if from_sidecar.exists() {
            move_storage_path(&from_sidecar, &path_with_suffix(to, suffix))?;
        }
    }
    Ok(())
}

fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn move_path(from: &Path, to: &Path) -> anyhow::Result<()> {
    if to.exists() {
        let from_metadata = std::fs::symlink_metadata(from)?;
        let to_metadata = std::fs::symlink_metadata(to)?;
        if from_metadata.is_dir() && to_metadata.is_dir() {
            return move_directory_contents(from, to);
        }
        if from_metadata.is_file() && to_metadata.is_file() && files_equal(from, to)? {
            std::fs::remove_file(from)?;
            return Ok(());
        }
        anyhow::bail!(
            "storage migration destination already contains different data at {}",
            to.display()
        );
    }
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    let pending = migration_pending_path(to)?;
    if pending.exists() {
        remove_path(&pending)?;
    }
    copy_path(from, &pending)?;
    std::fs::rename(&pending, to)?;
    remove_path(from)?;
    Ok(())
}

fn migration_pending_path(destination: &Path) -> anyhow::Result<PathBuf> {
    let name = destination
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("storage migration destination has no file name"))?
        .to_string_lossy();
    Ok(destination.with_file_name(format!(".keeppeek-migration-{name}.partial")))
}

fn files_equal(left: &Path, right: &Path) -> anyhow::Result<bool> {
    use std::io::Read as _;

    let left_metadata = std::fs::metadata(left)?;
    let right_metadata = std::fs::metadata(right)?;
    if left_metadata.len() != right_metadata.len() {
        return Ok(false);
    }
    let mut left = std::fs::File::open(left)?;
    let mut right = std::fs::File::open(right)?;
    let mut left_buffer = [0u8; 64 * 1024];
    let mut right_buffer = [0u8; 64 * 1024];
    loop {
        let left_read = left.read(&mut left_buffer)?;
        let right_read = right.read(&mut right_buffer)?;
        if left_read != right_read || left_buffer[..left_read] != right_buffer[..left_read] {
            return Ok(false);
        }
        if left_read == 0 {
            return Ok(true);
        }
    }
}

fn copy_path(from: &Path, to: &Path) -> anyhow::Result<()> {
    let metadata = std::fs::symlink_metadata(from)?;
    if metadata.file_type().is_symlink() {
        anyhow::bail!(
            "storage migration does not follow symbolic links: {}",
            from.display()
        );
    }
    if metadata.is_file() {
        std::fs::copy(from, to)?;
        std::fs::set_permissions(to, metadata.permissions())?;
        return Ok(());
    }
    if !metadata.is_dir() {
        anyhow::bail!(
            "storage migration source {} has an unsupported file type",
            from.display()
        );
    }
    std::fs::create_dir(to)?;
    std::fs::set_permissions(to, metadata.permissions())?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        copy_path(&entry.path(), &to.join(entry.file_name()))?;
    }
    Ok(())
}

fn remove_path(path: &Path) -> anyhow::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        std::fs::remove_dir_all(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

pub fn set_camera_manufacturer(
    path: &Path,
    camera_ip: IpAddr,
    manufacturer: Option<&str>,
) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let mut root: toml::Table = toml::from_str(&text)?;
    let mut matches = Vec::new();

    for (namespace, namespace_value) in &root {
        if namespace == "storage" {
            continue;
        }
        let Some(cameras) = namespace_value.as_table() else {
            continue;
        };
        for (name, camera_value) in cameras {
            let matches_ip = camera_value
                .as_table()
                .and_then(|camera| camera.get("ip"))
                .and_then(toml::Value::as_str)
                .and_then(|ip| ip.parse::<IpAddr>().ok())
                .is_some_and(|ip| ip == camera_ip);
            if matches_ip {
                matches.push((namespace.clone(), name.clone()));
            }
        }
    }

    let (namespace, name) = match matches.as_slice() {
        [(namespace, name)] => (namespace, name),
        [] => anyhow::bail!("camera {camera_ip} was not found in the configuration"),
        _ => anyhow::bail!("camera {camera_ip} appears more than once in the configuration"),
    };
    let camera = root
        .get_mut(namespace)
        .and_then(toml::Value::as_table_mut)
        .and_then(|cameras| cameras.get_mut(name))
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| anyhow::anyhow!("camera {camera_ip} is not a configuration table"))?;
    if let Some(manufacturer) = manufacturer {
        camera.insert(
            "manufacturer".to_owned(),
            toml::Value::String(manufacturer.to_owned()),
        );
    } else {
        camera.remove("manufacturer");
    }

    write_private_file_atomically(path, toml::to_string_pretty(&root)?.as_bytes())?;
    Ok(())
}

pub fn upsert_camera(path: &Path, config: &CameraConfig) -> anyhow::Result<String> {
    let text = std::fs::read_to_string(path)?;
    let mut root: toml::Table = toml::from_str(&text)?;
    let secrets = load_secrets(path)?;
    let defaults = camera_defaults_from_table(&root, &secrets)?;
    let existing = camera_locations(&root, config.ip)?;
    let (namespace, name) = if let Some((namespace, name)) = existing {
        (namespace, name)
    } else {
        let cameras = root
            .entry("cameras".to_owned())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .ok_or_else(|| anyhow::anyhow!("cameras is not a configuration table"))?;
        let name = unique_camera_key(cameras, config);
        cameras.insert(name.clone(), toml::Value::Table(toml::Table::new()));
        ("cameras".to_owned(), name)
    };
    let camera = root
        .get_mut(&namespace)
        .and_then(toml::Value::as_table_mut)
        .and_then(|cameras| cameras.get_mut(&name))
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| anyhow::anyhow!("camera {} is not a configuration table", config.ip))?;
    let original = camera.clone();

    const MANAGED_CAMERA_KEYS: &[&str] = &[
        "ip",
        "name",
        "display_name",
        "manufacturer",
        "onvif_port",
        "http_port",
        "main_rtsp_url",
        "sub_rtsp_url",
        "uid",
        "backend",
        "transport",
    ];
    for key in MANAGED_CAMERA_KEYS {
        camera.remove(*key);
    }
    set_camera_credential(
        camera,
        &original,
        "username",
        &config.username,
        &defaults.username,
        &secrets,
    )?;
    set_camera_credential(
        camera,
        &original,
        "password",
        &config.password,
        &defaults.password,
        &secrets,
    )?;
    let serialized: toml::Table = toml::Value::try_from(config)?
        .as_table()
        .cloned()
        .unwrap_or_default();
    for (key, value) in serialized {
        if !matches!(key.as_str(), "name" | "username" | "password") {
            let value = preserve_secret_reference(original.get(&key), value, &secrets)?;
            camera.insert(key, value);
        }
    }

    write_private_file_atomically(path, toml::to_string_pretty(&root)?.as_bytes())?;
    Ok(name)
}

fn set_camera_credential(
    camera: &mut toml::Table,
    original: &toml::Table,
    key: &str,
    next: &str,
    default: &str,
    secrets: &Secrets,
) -> anyhow::Result<()> {
    if let Some(existing) = original.get(key).and_then(toml::Value::as_str)
        && contains_secret_reference(existing)
        && resolve_secret_references_loaded(existing, secrets)? == next
    {
        camera.insert(key.to_owned(), toml::Value::String(existing.to_owned()));
        return Ok(());
    }
    if next.is_empty() || next == default {
        camera.remove(key);
    } else {
        camera.insert(key.to_owned(), toml::Value::String(next.to_owned()));
    }
    Ok(())
}

fn preserve_secret_reference(
    existing: Option<&toml::Value>,
    next: toml::Value,
    secrets: &Secrets,
) -> anyhow::Result<toml::Value> {
    if let (Some(existing), Some(next_string)) =
        (existing.and_then(toml::Value::as_str), next.as_str())
        && contains_secret_reference(existing)
        && resolve_secret_references_loaded(existing, secrets)? == next_string
    {
        return Ok(toml::Value::String(existing.to_owned()));
    }
    Ok(next)
}

pub fn remove_camera(path: &Path, camera_ip: IpAddr) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let mut root: toml::Table = toml::from_str(&text)?;
    let Some((namespace, name)) = camera_locations(&root, camera_ip)? else {
        anyhow::bail!("camera {camera_ip} was not found in the configuration");
    };
    root.get_mut(&namespace)
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| anyhow::anyhow!("camera namespace {namespace} is not a table"))?
        .remove(&name);
    write_private_file_atomically(path, toml::to_string_pretty(&root)?.as_bytes())?;
    Ok(())
}

/// Returns a raw camera secret reference for API display, or the resolved value.
pub fn camera_reference_or_value(
    path: &Path,
    camera_ip: IpAddr,
    key: &str,
    resolved: &str,
) -> anyhow::Result<String> {
    let text = std::fs::read_to_string(path)?;
    let root: toml::Table = toml::from_str(&text)?;
    let Some((namespace, name)) = camera_locations(&root, camera_ip)? else {
        return Ok(resolved.to_owned());
    };
    Ok(root
        .get(&namespace)
        .and_then(toml::Value::as_table)
        .and_then(|cameras| cameras.get(&name))
        .and_then(toml::Value::as_table)
        .and_then(|camera| camera.get(key))
        .and_then(toml::Value::as_str)
        .filter(|value| contains_secret_reference(value))
        .unwrap_or(resolved)
        .to_owned())
}

fn camera_locations(
    root: &toml::Table,
    camera_ip: IpAddr,
) -> anyhow::Result<Option<(String, String)>> {
    let mut matches = Vec::new();
    for (namespace, namespace_value) in root {
        if namespace == "storage" {
            continue;
        }
        let Some(cameras) = namespace_value.as_table() else {
            continue;
        };
        for (name, camera_value) in cameras {
            let matches_ip = camera_value
                .as_table()
                .and_then(|camera| camera.get("ip"))
                .and_then(toml::Value::as_str)
                .and_then(|ip| ip.parse::<IpAddr>().ok())
                .is_some_and(|ip| ip == camera_ip);
            if matches_ip {
                matches.push((namespace.clone(), name.clone()));
            }
        }
    }
    match matches.as_slice() {
        [(namespace, name)] => Ok(Some((namespace.clone(), name.clone()))),
        [] => Ok(None),
        _ => anyhow::bail!("camera {camera_ip} appears more than once in the configuration"),
    }
}

fn unique_camera_key(cameras: &toml::Table, config: &CameraConfig) -> String {
    let base = config
        .display_name()
        .map(sanitize_camera_key)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| format!("camera_{}", config.ip.to_string().replace(['.', ':'], "_")));
    if !cameras.contains_key(&base) {
        return base;
    }
    for suffix in 2.. {
        let name = format!("{base}_{suffix}");
        if !cameras.contains_key(&name) {
            return name;
        }
    }
    unreachable!("numeric camera key suffixes are unbounded")
}

fn sanitize_camera_key(name: &str) -> String {
    let key = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    key.trim_matches('_').to_owned()
}

pub(crate) fn write_private_file_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config");
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(".{filename}.{unique}.tmp"));
    #[cfg(not(unix))]
    let permissions = std::fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions());

    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    #[cfg(not(unix))]
    if let Some(permissions) = permissions {
        std::fs::set_permissions(&temporary, permissions)?;
    }
    std::fs::rename(temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{CatalogRecording, RecordingCatalog};

    fn create_migration_catalog(catalog_path: &Path, recording_path: &Path) {
        let catalog = RecordingCatalog::open(catalog_path).unwrap();
        let handle = catalog.handle();
        handle
            .upsert_recording(CatalogRecording {
                id: "migration-recording".to_owned(),
                stream_id: "front_gate/main".to_owned(),
                source_id: Some("front_gate".to_owned()),
                logical_stream_id: Some("main".to_owned()),
                started_at_ms: 1_000,
                ended_at_ms: Some(2_000),
                path: recording_path.to_string_lossy().into_owned(),
                init_offset: 0,
                init_len: 8,
                finalized: true,
            })
            .unwrap();
        handle
            .update_recording_path("migration-recording", recording_path, true)
            .unwrap();
        drop(handle);
        catalog.shutdown();
    }

    fn assert_migrated_catalog_path(catalog_path: &Path, expected_path: &Path) {
        let catalog = RecordingCatalog::open(catalog_path).unwrap();
        let handle = catalog.handle();
        let candidate = handle.claim_cleanup_candidate().unwrap().unwrap();
        assert_eq!(candidate.path, expected_path);
        drop(handle);
        catalog.shutdown();
    }

    #[test]
    fn battery_wake_config_is_not_treated_as_camera_configuration() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-battery-wake-config-{}",
            rand::random::<u64>()
        ));
        let path = directory.join("config.toml");
        write_private_file(
            &path,
            br#"
                [battery_wake]
                enabled = true
                bind = "192.0.2.1"
                middleman_port = 9999
                register_port = 58200
                heartbeat_secs = 20
                stale_after_secs = 80

                [cameras.battery]
                ip = "192.0.2.10"
                username = "operator"
                password = "secret"
                uid = "BATTERYCAMERA0001"
                backend = "reo-proto"
                transport = "udp"
            "#,
        )
        .unwrap();

        let config: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        config.battery_wake.validate().unwrap();
        let cameras = load_cameras(&path).unwrap();
        assert_eq!(cameras.len(), 1);
        assert_eq!(cameras["cameras"].len(), 1);

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn access_config_is_not_treated_as_camera_configuration() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-access-config-{}", uuid::Uuid::new_v4()));
        let path = directory.join("config.toml");
        write_private_file(
            &path,
            br#"
                [access]
                local_networks = ["127.0.0.0/8", "192.168.1.0/24"]
                trusted_proxies = ["127.0.0.1/32"]
                require_secure_remote = true
                failed_authentication_limit = 5
            "#,
        )
        .unwrap();

        let cameras = load_cameras(&path).unwrap();
        assert!(cameras.is_empty());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn homekit_config_is_not_treated_as_camera_configuration() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-homekit-config-{}", rand::random::<u64>()));
        let path = directory.join("config.toml");
        write_private_file(
            &path,
            br#"
                [homekit]
                enabled = true
                bind = "0.0.0.0"
                name = "KeepPeek"
                port = 32010

                [cameras.front]
                ip = "192.0.2.10"
                username = "operator"
                password = "secret"
            "#,
        )
        .unwrap();

        let cameras = load_cameras(&path).unwrap();
        assert_eq!(cameras.len(), 1);
        assert_eq!(cameras["cameras"].len(), 1);

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn service_logging_config_is_not_treated_as_camera_configuration() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-service-logging-config-{}",
            rand::random::<u64>()
        ));
        let path = directory.join("config.toml");
        write_private_file(
            &path,
            br#"
                [logging]
                service = "event_log"

                [cameras.front]
                ip = "192.0.2.10"
                username = "operator"
                password = "secret"
            "#,
        )
        .unwrap();

        let config: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.logging.service, ServiceLogDestination::EventLog);
        let cameras = load_cameras(&path).unwrap();
        assert_eq!(cameras.len(), 1);
        assert_eq!(cameras["cameras"].len(), 1);

        std::fs::remove_dir_all(directory).unwrap();
    }
    use crate::cameras::{CameraBackend, CameraTransport};

    #[cfg(unix)]
    #[test]
    fn private_files_are_owner_only() {
        use std::{
            os::unix::fs::PermissionsExt,
            time::{SystemTime, UNIX_EPOCH},
        };

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-private-file-{}-{unique}",
            std::process::id()
        ));
        let path = directory.join("config.toml");

        write_private_file(&path, b"host = 'localhost'").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn camera_profile_defaults_to_auto_backend_and_tcp_transport() {
        let config: CameraConfig = toml::from_str(
            r#"
                ip = "192.168.1.10"
                username = "operator"
                password = "secret"
            "#,
        )
        .unwrap();

        assert_eq!(config.backend, CameraBackend::Auto);
        assert_eq!(config.transport, CameraTransport::Tcp);
        assert_eq!(config.http_port, None);
        assert_eq!(config.uid, None);
    }

    #[test]
    fn camera_config_debug_output_is_redacted() {
        let config: CameraConfig = toml::from_str(
            r#"
                ip = "192.168.1.10"
                username = "operator"
                password = "camera-password"
                main_rtsp_url = "rtsp://private-camera.internal/main"
                uid = "PRIVATE-CAMERA-UID"
            "#,
        )
        .unwrap();

        let debug = format!("{config:?}");

        assert!(debug.contains("username_configured: true"));
        assert!(debug.contains("password_configured: true"));
        assert!(!debug.contains("operator"));
        assert!(!debug.contains("camera-password"));
        assert!(!debug.contains("private-camera.internal"));
        assert!(!debug.contains("PRIVATE-CAMERA-UID"));
    }

    #[test]
    fn camera_credentials_resolve_from_defaults_and_camera_references() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-secrets-{}", rand::random::<u64>()));
        let path = directory.join("config.toml");
        write_private_file(
            &path,
            br#"
                [camera_defaults]
                username = "{secret:CAMERA_USERNAME}"
                password = "{secret:CAMERA_PASSWORD}"

                [cameras.front]
                ip = "192.0.2.10"
                password = "{secret:FRONT_CAMERA_PASSWORD}"
            "#,
        )
        .unwrap();
        write_private_file(
            &secrets_path(&path),
            br#"
                CAMERA_USERNAME = "default-user"
                CAMERA_PASSWORD = "default-password"
                FRONT_CAMERA_PASSWORD = " specific password "
                HOME_ASSISTANT_TOKEN = "integration-token"
            "#,
        )
        .unwrap();

        let cameras = load_cameras(&path).unwrap();
        let camera = &cameras["cameras"][0];
        assert_eq!(camera.username, "default-user");
        assert_eq!(camera.password, " specific password ");
        let secrets = load_secrets(&path).unwrap();
        assert_eq!(
            secrets.0.get("HOME_ASSISTANT_TOKEN").map(String::as_str),
            Some("integration-token")
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn inline_camera_credentials_override_referenced_defaults() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-inline-secrets-{}", rand::random::<u64>()));
        let path = directory.join("config.toml");
        write_private_file(
            &path,
            r#"
                [camera_defaults]
                username = "{secret:CAMERA_USERNAME}"
                password = "{secret:CAMERA_PASSWORD}"

                [cameras.front]
                ip = "192.0.2.10"
                username = "legacy-user"
                password = "legacy-password"
            "#
            .as_bytes(),
        )
        .unwrap();
        write_private_file(
            &secrets_path(&path),
            b"CAMERA_USERNAME = \"default-user\"\nCAMERA_PASSWORD = \"default-password\"\n",
        )
        .unwrap();

        let cameras = load_cameras(&path).unwrap();
        assert_eq!(cameras["cameras"][0].username, "legacy-user");
        assert_eq!(cameras["cameras"][0].password, "legacy-password");

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn secrets_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let directory = std::env::temp_dir().join(format!(
            "keeppeek-private-secrets-{}",
            rand::random::<u64>()
        ));
        let path = directory.join("config.toml");
        write_private_file(&secrets_path(&path), b"CAMERA_USERNAME = \"operator\"\n").unwrap();
        load_secrets(&path).unwrap();

        let mode = std::fs::metadata(secrets_path(&path))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn first_start_creates_the_secrets_template() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-secrets-template-{}",
            rand::random::<u64>()
        ));
        let path = directory.join("config.toml");

        let secrets = ensure_secrets_file(&path).unwrap();

        assert!(secrets.0.is_empty());
        let template = std::fs::read_to_string(secrets_path(&path)).unwrap();
        assert!(template.contains("CAMERA_USERNAME"));
        assert!(template.contains("FRONT_CAMERA_PASSWORD"));
        assert!(template.contains("HOME_ASSISTANT_TOKEN"));

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn generated_access_key_is_stored_only_in_the_secret_file() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-access-key-secret-{}",
            rand::random::<u64>()
        ));
        let path = directory.join("config.toml");
        let mut secrets = ensure_secrets_file(&path).unwrap();

        let access_key = ensure_access_key_secret(&path, &mut secrets).unwrap();

        let secret_file = std::fs::read_to_string(secrets_path(&path)).unwrap();
        assert!(secret_file.contains("KEEPPEEK_ACCESS_KEY"));
        assert!(secret_file.contains(&access_key.canonical()));
        let mut config = toml::Table::new();
        config.insert(
            "access_key".to_owned(),
            toml::Value::String("{secret:KEEPPEEK_ACCESS_KEY}".to_owned()),
        );
        write_private_file(&path, toml::to_string_pretty(&config).unwrap().as_bytes()).unwrap();
        let raw_config = std::fs::read_to_string(&path).unwrap();
        assert!(raw_config.contains("{secret:KEEPPEEK_ACCESS_KEY}"));
        assert!(!raw_config.contains(&access_key.canonical()));
        let loaded = load_config(&path).unwrap();
        assert_eq!(loaded.access_key, access_key);

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn existing_access_key_can_be_migrated_to_the_secret_file() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-access-key-migration-{}",
            rand::random::<u64>()
        ));
        let path = directory.join("config.toml");
        let access_key = AccessKey::parse("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let mut secrets = ensure_secrets_file(&path).unwrap();

        store_access_key_secret(&path, &mut secrets, access_key).unwrap();

        let saved = load_secrets(&path).unwrap();
        assert_eq!(
            saved.0.get(ACCESS_KEY_SECRET).map(String::as_str),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn access_key_rotation_replaces_only_the_owner_only_secret() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-access-key-rotation-{}",
            rand::random::<u64>()
        ));
        let path = directory.join("config.toml");
        write_private_file(&path, b"access_key = \"{secret:KEEPPEEK_ACCESS_KEY}\"\n").unwrap();
        write_private_file(
            &secrets_path(&path),
            b"CAMERA_PASSWORD = \"camera-secret\"\nKEEPPEEK_ACCESS_KEY = \"550e8400-e29b-41d4-a716-446655440000\"\n",
        )
        .unwrap();

        let rotated = rotate_access_key_secret(&path).unwrap();

        assert_ne!(
            rotated,
            AccessKey::parse("550e8400-e29b-41d4-a716-446655440000").unwrap()
        );
        let secrets = load_secrets(&path).unwrap();
        assert_eq!(
            secrets.0.get(ACCESS_KEY_SECRET).map(String::as_str),
            Some(rotated.canonical().as_str())
        );
        assert_eq!(
            secrets.0.get("CAMERA_PASSWORD").map(String::as_str),
            Some("camera-secret")
        );
        let raw_config = std::fs::read_to_string(&path).unwrap();
        assert!(raw_config.contains("{secret:KEEPPEEK_ACCESS_KEY}"));
        assert!(!raw_config.contains(&rotated.canonical()));

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn secret_references_support_interpolation_url_encoding_and_environment_precedence() {
        let secrets = Secrets(BTreeMap::from([
            ("CAMERA_USER".to_owned(), "viewer".to_owned()),
            (
                "CAMERA_PASSWORD".to_owned(),
                "p@ss word/with+specials".to_owned(),
            ),
        ]));
        let resolved = resolve_secret_references_with(
            "rtsp://{secret:CAMERA_USER}:{secret:CAMERA_PASSWORD|url}@camera.local/live",
            &secrets,
            |key| Ok((key == "CAMERA_USER").then(|| "operator".to_owned())),
        )
        .unwrap();

        assert_eq!(
            resolved,
            "rtsp://operator:p%40ss%20word%2Fwith%2Bspecials@camera.local/live"
        );
    }

    #[test]
    fn missing_and_malformed_secret_references_fail_without_values() {
        let secrets = Secrets(BTreeMap::from([(
            "KNOWN_SECRET".to_owned(),
            "must-not-appear".to_owned(),
        )]));

        let missing =
            resolve_secret_references_with("{secret:MISSING_SECRET}", &secrets, |_| Ok(None))
                .unwrap_err()
                .to_string();
        assert!(missing.contains("MISSING_SECRET"));
        assert!(!missing.contains("must-not-appear"));
        assert!(
            resolve_secret_references_with("{secret:KNOWN_SECRET", &secrets, |_| Ok(None))
                .unwrap_err()
                .to_string()
                .contains("malformed secret reference")
        );
    }

    #[test]
    fn nested_secrets_toml_is_rejected_without_echoing_values() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-nested-secrets-{}", rand::random::<u64>()));
        let path = directory.join("config.toml");
        write_private_file(
            &secrets_path(&path),
            b"[nested]\nPASSWORD = \"must-not-appear\"\n",
        )
        .unwrap();

        let error = match load_secrets(&path) {
            Ok(_) => panic!("nested secrets must be rejected"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("flat string-to-string TOML table"));
        assert!(!error.contains("must-not-appear"));

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn settings_round_trip_preserves_references_and_exposes_no_resolved_values() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-secret-roundtrip-{}",
            rand::random::<u64>()
        ));
        let path = directory.join("config.toml");
        write_private_file(
            &path,
            br#"
                host = "{secret:BIND_HOST}"
                port = 8081

                [storage]
                medium_term_path = "{secret:RECORDING_PATH}"
                long_term_path = "{secret:RECORDING_PATH}"
            "#,
        )
        .unwrap();
        write_private_file(
            &secrets_path(&path),
            br#"
                BIND_HOST = "127.0.0.1"
                RECORDING_PATH = "/private/recordings"
            "#,
        )
        .unwrap();

        let loaded = load_config(&path).unwrap();
        assert_eq!(loaded.host, "127.0.0.1");
        assert_eq!(
            loaded.storage.long_term_path.as_deref(),
            Some("/private/recordings")
        );
        assert_eq!(
            loaded.reference_or_value(&["host"], &loaded.host),
            "{secret:BIND_HOST}"
        );

        update_settings(&path, &loaded).unwrap();

        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("host = \"{secret:BIND_HOST}\""));
        assert!(saved.contains("long_term_path = \"{secret:RECORDING_PATH}\""));
        assert!(!saved.contains("127.0.0.1"));
        assert!(!saved.contains("/private/recordings"));

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn camera_profile_parses_explicit_backend_and_transport() {
        let config: CameraConfig = toml::from_str(
            r#"
                ip = "192.168.1.10"
                username = "operator"
                password = "secret"
                backend = "reo-proto"
                transport = "udp"
                http_port = 8080
                uid = "95270001UVBK2KJ6"
            "#,
        )
        .unwrap();

        assert_eq!(config.backend, CameraBackend::ReoProto);
        assert_eq!(config.transport, CameraTransport::Udp);
        assert_eq!(config.http_port, Some(8080));
        assert_eq!(config.uid.as_deref(), Some("95270001UVBK2KJ6"));
    }

    #[test]
    fn camera_display_name_does_not_replace_stable_name() {
        let config: CameraConfig = toml::from_str(
            r#"
                ip = "192.168.1.10"
                display_name = "North Garden"
                username = "operator"
                password = "secret"
            "#,
        )
        .unwrap();

        assert_eq!(config.name, None);
        assert_eq!(config.display_name(), Some("North Garden"));
    }

    #[test]
    fn settings_update_preserves_camera_entries_and_storage_paths() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-settings-{}", rand::random::<u64>()));
        let path = directory.join("config.toml");
        write_private_file(
            &path,
            br#"
                host = "0.0.0.0"
                port = 8081
                access_key = "550e8400-e29b-41d4-a716-446655440000"
                unrelated_setting = "preserved"

                [direct_card]
                allowed_origins = ["https://home.example.net"]

                [storage]
                medium_term_path = "/media/keeppeek"
                long_term_path = "/archive/keeppeek"
                storage_extension = "preserved"

                [cameras.front]
                ip = "192.0.2.10"
                username = "operator"
                password = "secret"
            "#,
        )
        .unwrap();
        let settings = Config {
            host: "127.0.0.1".to_owned(),
            port: 3200,
            storage: StorageToml {
                medium_term_path: None,
                long_term_path: None,
                recording_catalog_path: Some("/metadata/recordings.db".to_owned()),
                event_thumbnail_path: Some("/metadata/event-thumbnails".to_owned()),
                event_thumbnail_max_mb: 512,
                short_term_secs: 30,
                medium_term_secs: 120,
                flush_interval_secs: 15,
                write_buffer_bytes: 16_384,
                long_term_max_gb: 24,
                minimum_free_gb: 8,
                maximum_used_percent: Some(85),
                warning_free_gb: 12,
                critical_free_gb: 8,
                cleanup_hysteresis_gb: 2,
            },
            ..Config::default()
        };

        let updated = update_settings(&path, &settings).unwrap();

        assert_eq!(updated.host, "127.0.0.1");
        assert_eq!(updated.port, 3200);
        assert_eq!(
            updated.access_key.canonical(),
            "550e8400-e29b-41d4-a716-446655440000"
        );
        assert_eq!(
            updated.storage.medium_term_path.as_deref(),
            Some("/media/keeppeek")
        );
        assert_eq!(
            updated.storage.long_term_path.as_deref(),
            Some("/archive/keeppeek")
        );
        assert_eq!(
            updated.storage.recording_catalog_path.as_deref(),
            Some("/metadata/recordings.db")
        );
        assert_eq!(
            updated.storage.event_thumbnail_path.as_deref(),
            Some("/metadata/event-thumbnails")
        );
        assert_eq!(updated.storage.event_thumbnail_max_mb, 512);
        let saved: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved["unrelated_setting"].as_str(), Some("preserved"));
        assert_eq!(
            saved["access_key"].as_str(),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
        assert_eq!(
            saved["direct_card"]["allowed_origins"][0].as_str(),
            Some("https://home.example.net")
        );
        assert_eq!(
            saved["storage"]["storage_extension"].as_str(),
            Some("preserved")
        );
        assert_eq!(
            saved["cameras"]["front"]["password"].as_str(),
            Some("secret")
        );
        assert_eq!(saved["storage"]["short_term_secs"].as_integer(), Some(30));
        assert_eq!(saved["storage"]["long_term_max_gb"].as_integer(), Some(24));
        assert_eq!(saved["storage"]["minimum_free_gb"].as_integer(), Some(8));
        assert_eq!(
            saved["storage"]["maximum_used_percent"].as_integer(),
            Some(85)
        );
        assert_eq!(
            saved["storage"]["recording_catalog_path"].as_str(),
            Some("/metadata/recordings.db")
        );
        assert_eq!(
            saved["storage"]["event_thumbnail_path"].as_str(),
            Some("/metadata/event-thumbnails")
        );
        assert_eq!(
            saved["storage"]["event_thumbnail_max_mb"].as_integer(),
            Some(512)
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn direct_card_origins_are_exact_canonical_http_origins() {
        DirectCardConfig {
            allowed_origins: vec![
                "https://home.example.net".to_owned(),
                "http://127.0.0.1:4174".to_owned(),
            ],
        }
        .validate()
        .unwrap();

        for origin in [
            "*",
            "ftp://home.example.net",
            "https://home.example.net/",
            "https://home.example.net/card",
            "http://home.example.net:80",
        ] {
            assert!(
                DirectCardConfig {
                    allowed_origins: vec![origin.to_owned()],
                }
                .validate()
                .is_err(),
                "origin {origin} must be rejected"
            );
        }
        assert!(
            DirectCardConfig {
                allowed_origins: vec![
                    "https://home.example.net".to_owned(),
                    "https://home.example.net".to_owned(),
                ],
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn pending_storage_migration_moves_existing_recordings_and_clears_marker() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-storage-move-{}", rand::random::<u64>()));
        let current = directory.join("current");
        let next = directory.join("next");
        let recording = current.join("front_gate/main/2026-08-12/12/0000.mp4");
        let catalog = current.join("recordings.db");
        let thumbnail = current.join(".event-thumbnails/event-1.jpg");
        std::fs::create_dir_all(recording.parent().unwrap()).unwrap();
        std::fs::create_dir_all(thumbnail.parent().unwrap()).unwrap();
        std::fs::write(&recording, b"recording").unwrap();
        create_migration_catalog(&catalog, &recording);
        std::fs::write(&thumbnail, b"thumbnail").unwrap();

        let migration = StorageMigration::between_with_metadata(
            StorageMigrationPaths::new(
                &current,
                &current,
                &catalog,
                &current.join(".event-thumbnails"),
            ),
            StorageMigrationPaths::new(
                &next,
                &next,
                &next.join("recordings.db"),
                &next.join(".event-thumbnails"),
            ),
        )
        .unwrap()
        .unwrap();
        let mut root = toml::Table::new();
        root.insert(
            STORAGE_MIGRATION_SECTION.to_owned(),
            toml::Value::try_from(&migration).unwrap(),
        );

        apply_pending_storage_migration(&mut root).unwrap();

        assert!(!root.contains_key(STORAGE_MIGRATION_SECTION));
        assert!(!current.exists());
        assert_eq!(
            std::fs::read(next.join("front_gate/main/2026-08-12/12/0000.mp4")).unwrap(),
            b"recording"
        );
        assert_migrated_catalog_path(
            &next.join("recordings.db"),
            &next.join("front_gate/main/2026-08-12/12/0000.mp4"),
        );
        assert_eq!(
            std::fs::read(next.join(".event-thumbnails/event-1.jpg")).unwrap(),
            b"thumbnail"
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn pending_storage_migration_moves_custom_metadata_paths() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-custom-storage-move-{}",
            rand::random::<u64>()
        ));
        let current_recordings = directory.join("current-recordings");
        let next_recordings = directory.join("next-recordings");
        let current_catalog = directory.join("current-metadata/recordings.db");
        let next_catalog = directory.join("next-metadata/recordings.db");
        let current_thumbnails = directory.join("current-thumbnails");
        let next_thumbnails = directory.join("next-thumbnails");
        std::fs::create_dir_all(current_recordings.join("front_gate/main")).unwrap();
        std::fs::create_dir_all(current_catalog.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&current_thumbnails).unwrap();
        std::fs::write(
            current_recordings.join("front_gate/main/0000.mp4"),
            b"recording",
        )
        .unwrap();
        create_migration_catalog(
            &current_catalog,
            &current_recordings.join("front_gate/main/0000.mp4"),
        );
        std::fs::write(current_thumbnails.join("event-1.jpg"), b"thumbnail").unwrap();

        let migration = StorageMigration::between_with_metadata(
            StorageMigrationPaths::new(
                &current_recordings,
                &current_recordings,
                &current_catalog,
                &current_thumbnails,
            ),
            StorageMigrationPaths::new(
                &next_recordings,
                &next_recordings,
                &next_catalog,
                &next_thumbnails,
            ),
        )
        .unwrap()
        .unwrap();
        let mut root = toml::Table::new();
        root.insert(
            STORAGE_MIGRATION_SECTION.to_owned(),
            toml::Value::try_from(&migration).unwrap(),
        );

        apply_pending_storage_migration(&mut root).unwrap();
        migration.apply().unwrap();

        assert!(!root.contains_key(STORAGE_MIGRATION_SECTION));
        assert_eq!(
            std::fs::read(next_recordings.join("front_gate/main/0000.mp4")).unwrap(),
            b"recording"
        );
        assert_migrated_catalog_path(
            &next_catalog,
            &next_recordings.join("front_gate/main/0000.mp4"),
        );
        assert_eq!(
            std::fs::read(next_thumbnails.join("event-1.jpg")).unwrap(),
            b"thumbnail"
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn storage_migration_replays_a_committed_copy_before_source_cleanup() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-storage-copy-replay-{}",
            rand::random::<u64>()
        ));
        let current = directory.join("current");
        let next = directory.join("next");
        let source = current.join("front/main/recording.mp4");
        let destination = next.join("front/main/recording.mp4");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::write(&source, b"complete recording").unwrap();
        std::fs::write(&destination, b"complete recording").unwrap();
        let migration = StorageMigration::between(&current, &next, &current, &next)
            .unwrap()
            .unwrap();

        migration.apply().unwrap();
        migration.apply().unwrap();

        assert!(!current.exists());
        assert_eq!(std::fs::read(destination).unwrap(), b"complete recording");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn pending_storage_migration_moves_renamed_metadata_inside_recording_root() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-renamed-storage-move-{}",
            rand::random::<u64>()
        ));
        let current = directory.join("current");
        let next = directory.join("next");
        let current_catalog = current.join("metadata/recordings.db");
        let next_catalog = next.join("catalog/current.db");
        let current_thumbnails = current.join("event-images");
        let next_thumbnails = next.join("retained-images");
        std::fs::create_dir_all(current.join("front_gate/main")).unwrap();
        std::fs::create_dir_all(current_catalog.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&current_thumbnails).unwrap();
        std::fs::write(current.join("front_gate/main/0000.mp4"), b"recording").unwrap();
        create_migration_catalog(&current_catalog, &current.join("front_gate/main/0000.mp4"));
        std::fs::write(current_thumbnails.join("event-1.jpg"), b"thumbnail").unwrap();

        let migration = StorageMigration::between_with_metadata(
            StorageMigrationPaths::new(&current, &current, &current_catalog, &current_thumbnails),
            StorageMigrationPaths::new(&next, &next, &next_catalog, &next_thumbnails),
        )
        .unwrap()
        .unwrap();
        let mut root = toml::Table::new();
        root.insert(
            STORAGE_MIGRATION_SECTION.to_owned(),
            toml::Value::try_from(&migration).unwrap(),
        );

        apply_pending_storage_migration(&mut root).unwrap();

        assert_eq!(
            std::fs::read(next.join("front_gate/main/0000.mp4")).unwrap(),
            b"recording"
        );
        assert_migrated_catalog_path(&next_catalog, &next.join("front_gate/main/0000.mp4"));
        assert_eq!(
            std::fs::read(next_thumbnails.join("event-1.jpg")).unwrap(),
            b"thumbnail"
        );
        assert!(!next.join("metadata/recordings.db").exists());
        assert!(!next.join("event-images/event-1.jpg").exists());

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn storage_settings_can_start_new_without_moving_existing_recordings() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-storage-start-new-{}",
            rand::random::<u64>()
        ));
        let config_path = directory.join("config.toml");
        let current = directory.join("current");
        let next = directory.join("next");
        std::fs::create_dir_all(&current).unwrap();
        std::fs::write(current.join("existing.mp4"), b"existing recording").unwrap();
        write_private_file(
            &config_path,
            format!("[storage]\nmedium_term_path = {current:?}\nlong_term_path = {current:?}\n")
                .as_bytes(),
        )
        .unwrap();
        let settings = Config {
            host: "0.0.0.0".to_owned(),
            port: 8081,
            storage: StorageToml {
                medium_term_path: Some(next.to_string_lossy().into_owned()),
                long_term_path: Some(next.to_string_lossy().into_owned()),
                ..StorageToml::default()
            },
            ..Config::default()
        };
        let migration = StorageMigration::between(&current, &next, &current, &next)
            .unwrap()
            .unwrap();

        update_settings_with_migration(&config_path, &settings, Some(&migration)).unwrap();
        update_settings_with_migration(&config_path, &settings, None).unwrap();

        let saved: toml::Table =
            toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert!(!saved.contains_key(STORAGE_MIGRATION_SECTION));
        assert_eq!(
            std::fs::read(current.join("existing.mp4")).unwrap(),
            b"existing recording"
        );
        assert!(!next.exists());

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn storage_migration_rejects_splitting_one_current_path() {
        let current = PathBuf::from("/recordings");
        let error = StorageMigration::between(
            &current,
            Path::new("/medium-recordings"),
            &current,
            Path::new("/long-recordings"),
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("cannot split one current storage path")
        );
    }

    #[test]
    fn camera_manufacturer_override_is_persisted_and_can_be_cleared() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-manufacturer-{}", rand::random::<u64>()));
        let path = directory.join("cameras.toml");
        write_private_file(
            &path,
            br#"
                [cameras.back_yard]
                ip = "192.0.2.10"
                username = "operator"
                password = "secret"
            "#,
        )
        .unwrap();

        set_camera_manufacturer(&path, "192.0.2.10".parse().unwrap(), Some("Hikvision")).unwrap();
        let saved: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            saved["cameras"]["back_yard"]["manufacturer"].as_str(),
            Some("Hikvision")
        );

        set_camera_manufacturer(&path, "192.0.2.10".parse().unwrap(), None).unwrap();
        let cleared: toml::Table =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            cleared["cameras"]["back_yard"]
                .as_table()
                .is_some_and(|camera| !camera.contains_key("manufacturer"))
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn camera_upsert_preserves_unknown_fields_and_remove_deletes_the_entry() {
        let directory =
            std::env::temp_dir().join(format!("keeppeek-upsert-{}", rand::random::<u64>()));
        let path = directory.join("cameras.toml");
        write_private_file(
            &path,
            br#"
                [camera_defaults]
                username = "{secret:CAMERA_USERNAME}"

                [cameras.existing]
                ip = "192.0.2.10"
                password = "{secret:CAMERA_PASSWORD}"
                main_rtsp_url = "rtsp://{secret:CAMERA_HOST}/main"
                sub_rtsp_url = "rtsp://{secret:CAMERA_HOST}/sub"
                custom_option = "preserved"
            "#,
        )
        .unwrap();
        write_private_file(
            &secrets_path(&path),
            br#"
                CAMERA_USERNAME = "operator"
                CAMERA_PASSWORD = "camera-password"
                CAMERA_HOST = "192.0.2.10"
                SMTP_API_KEY = "mail-key"
            "#,
        )
        .unwrap();
        let config = CameraConfig {
            ip: "192.0.2.10".parse().unwrap(),
            name: Some("ignored".to_owned()),
            display_name: Some("Back Yard".to_owned()),
            manufacturer: Some("Hikvision".to_owned()),
            username: "operator".to_owned(),
            password: "camera-password".to_owned(),
            onvif_port: Some(80),
            http_port: None,
            main_rtsp_url: Some("rtsp://192.0.2.10/main".to_owned()),
            sub_rtsp_url: Some("rtsp://192.0.2.10/sub".to_owned()),
            uid: None,
            backend: CameraBackend::Retina,
            transport: CameraTransport::Tcp,
            record_generic_motion_events: false,
            recording_mode: Default::default(),
            event_recording_duration_secs: 60,
        };

        assert_eq!(upsert_camera(&path, &config).unwrap(), "existing");
        let saved: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            saved["cameras"]["existing"]["custom_option"].as_str(),
            Some("preserved")
        );
        assert_eq!(
            saved["cameras"]["existing"]["manufacturer"].as_str(),
            Some("Hikvision")
        );
        assert_eq!(
            saved["cameras"]["existing"]["main_rtsp_url"].as_str(),
            Some("rtsp://{secret:CAMERA_HOST}/main")
        );
        assert_eq!(
            saved["cameras"]["existing"]["sub_rtsp_url"].as_str(),
            Some("rtsp://{secret:CAMERA_HOST}/sub")
        );
        let saved_camera = saved["cameras"]["existing"].as_table().unwrap();
        assert!(!saved_camera.contains_key("username"));
        assert_eq!(
            saved_camera["password"].as_str(),
            Some("{secret:CAMERA_PASSWORD}")
        );
        let secrets = load_secrets(&path).unwrap();
        assert_eq!(
            secrets.0.get("SMTP_API_KEY").map(String::as_str),
            Some("mail-key")
        );
        let loaded = load_cameras(&path).unwrap();
        assert_eq!(loaded["cameras"][0].username, "operator");
        assert_eq!(loaded["cameras"][0].password, "camera-password");

        let mut changed = config;
        changed.password = "replacement-password".to_owned();
        upsert_camera(&path, &changed).unwrap();
        let changed_saved: toml::Table =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            changed_saved["cameras"]["existing"]["password"].as_str(),
            Some("replacement-password")
        );

        remove_camera(&path, "192.0.2.10".parse().unwrap()).unwrap();
        let saved: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            saved["cameras"]
                .as_table()
                .is_some_and(|cameras| !cameras.contains_key("existing"))
        );

        std::fs::remove_dir_all(directory).unwrap();
    }
}
