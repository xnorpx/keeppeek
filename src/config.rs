pub use crate::access::AccessKey;
use crate::cameras::CameraConfig;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
};
use url::Url;

const DEFAULT_CONFIG_NAME: &str = "config.toml";
const STORAGE_MIGRATION_SECTION: &str = "storage_migration";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    #[serde(default = "default_host")]
    pub host: String,

    #[serde(default = "default_port")]
    pub port: u16,

    #[serde(default)]
    pub access_key: AccessKey,

    #[serde(default)]
    pub direct_card: DirectCardConfig,

    #[serde(default)]
    pub storage: StorageToml,

    #[serde(default)]
    pub battery_wake: BatteryWakeConfig,

    #[serde(default)]
    pub logging: LoggingConfig,
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
        };
        if let Some((
            current_recording_catalog_path,
            next_recording_catalog_path,
            current_event_thumbnail_path,
            next_event_thumbnail_path,
        )) = metadata_paths
        {
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
        let mut moved = Vec::new();
        for route in self.routes() {
            if moved
                .iter()
                .any(|(from, to): &(PathBuf, PathBuf)| from == &route.from && to == &route.to)
            {
                continue;
            }
            move_storage_path(&route.from, &route.to)?;
            moved.push((route.from.clone(), route.to.clone()));
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
            direct_card: DirectCardConfig::default(),
            storage: StorageToml::default(),
            battery_wake: BatteryWakeConfig::default(),
            logging: LoggingConfig::default(),
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

    let (mut cfg, mut merged, existing_config) = if path.exists() {
        let text = std::fs::read_to_string(&path)?;
        let mut root: toml::Table = toml::from_str(&text)?;
        apply_pending_storage_migration(&mut root)?;
        let cfg: Config = toml::from_str(&toml::to_string(&root)?)?;
        (cfg, root, true)
    } else {
        (Config::default(), toml::Table::new(), false)
    };

    if let Some(access_key) = read_access_key_arg()?.filter(|key| !key.is_unset()) {
        cfg.access_key = access_key;
    }
    let generated_access_key = cfg.access_key.is_unset();
    if generated_access_key {
        cfg.access_key = AccessKey::generate();
    }
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
        merged.insert(key, value);
    }

    let text = toml::to_string_pretty(&merged)?;
    write_private_file(&path, text.as_bytes())?;

    if !existing_config {
        tracing::info!("created default config at {}", path.display());
    }
    if generated_access_key {
        println!("KeepPeek access key: {}", cfg.access_key.canonical());
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
    root.insert(
        "host".to_owned(),
        toml::Value::String(settings.host.clone()),
    );
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
        storage.insert(
            "medium_term_path".to_owned(),
            toml::Value::String(medium_term_path.clone()),
        );
    }
    if let Some(long_term_path) = &settings.storage.long_term_path {
        storage.insert(
            "long_term_path".to_owned(),
            toml::Value::String(long_term_path.clone()),
        );
    }
    match &settings.storage.recording_catalog_path {
        Some(recording_catalog_path) => {
            storage.insert(
                "recording_catalog_path".to_owned(),
                toml::Value::String(recording_catalog_path.clone()),
            );
        }
        None => {
            storage.remove("recording_catalog_path");
        }
    }
    match &settings.storage.event_thumbnail_path {
        Some(event_thumbnail_path) => {
            storage.insert(
                "event_thumbnail_path".to_owned(),
                toml::Value::String(event_thumbnail_path.clone()),
            );
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
    let updated: Config = toml::from_str(&serialized)?;
    write_private_file_atomically(path, serialized.as_bytes())?;
    Ok(updated)
}

pub fn load_cameras(path: &Path) -> anyhow::Result<HashMap<String, Vec<CameraConfig>>> {
    let text = std::fs::read_to_string(path)?;
    let root: toml::Value = toml::from_str(&text)?;

    let root_table = root
        .as_table()
        .ok_or_else(|| anyhow::anyhow!("cameras config is not a TOML table"))?;

    let mut result: HashMap<String, Vec<CameraConfig>> = HashMap::new();

    const RESERVED_SECTIONS: &[&str] = &[
        "storage",
        "battery_wake",
        "direct_card",
        "homekit",
        "logging",
        STORAGE_MIGRATION_SECTION,
    ];

    for (namespace, ns_value) in root_table {
        if RESERVED_SECTIONS.contains(&namespace.as_str()) {
            continue;
        }

        let Some(ns_table) = ns_value.as_table() else {
            continue;
        };

        let mut cameras = Vec::new();
        for (cam_name, cam_value) in ns_table {
            let mut config: CameraConfig = cam_value.clone().try_into()?;
            config.name = Some(cam_name.clone());
            cameras.push(config);
        }

        result.insert(namespace.clone(), cameras);
    }

    Ok(result)
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
        if destination.exists() {
            anyhow::bail!(
                "storage migration destination already contains {}",
                destination.display()
            );
        }
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
    if to.exists() {
        anyhow::bail!(
            "storage migration destination already contains {}",
            to.display()
        );
    }
    let parent = to.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    std::fs::rename(from, to)?;
    Ok(())
}

fn move_path(from: &Path, to: &Path) -> anyhow::Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    copy_path(from, to)?;
    remove_path(from)?;
    Ok(())
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

    const MANAGED_CAMERA_KEYS: &[&str] = &[
        "ip",
        "name",
        "display_name",
        "manufacturer",
        "username",
        "password",
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
    let serialized: toml::Table = toml::Value::try_from(config)?
        .as_table()
        .cloned()
        .unwrap_or_default();
    for (key, value) in serialized {
        if key != "name" {
            camera.insert(key, value);
        }
    }

    write_private_file_atomically(path, toml::to_string_pretty(&root)?.as_bytes())?;
    Ok(name)
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
    if let Some(permissions) = permissions {
        std::fs::set_permissions(&temporary, permissions)?;
    }
    std::fs::rename(temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        std::fs::write(&catalog, b"catalog").unwrap();
        std::fs::write(&thumbnail, b"thumbnail").unwrap();

        let migration = StorageMigration::between(&current, &next, &current, &next)
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
        assert_eq!(
            std::fs::read(next.join("recordings.db")).unwrap(),
            b"catalog"
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
        std::fs::write(&current_catalog, b"catalog").unwrap();
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

        assert!(!root.contains_key(STORAGE_MIGRATION_SECTION));
        assert_eq!(
            std::fs::read(next_recordings.join("front_gate/main/0000.mp4")).unwrap(),
            b"recording"
        );
        assert_eq!(std::fs::read(next_catalog).unwrap(), b"catalog");
        assert_eq!(
            std::fs::read(next_thumbnails.join("event-1.jpg")).unwrap(),
            b"thumbnail"
        );

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
        std::fs::write(&current_catalog, b"catalog").unwrap();
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
        assert_eq!(std::fs::read(next_catalog).unwrap(), b"catalog");
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
                [cameras.existing]
                ip = "192.0.2.10"
                username = "old"
                password = "old-secret"
                custom_option = "preserved"
            "#,
        )
        .unwrap();
        let config = CameraConfig {
            ip: "192.0.2.10".parse().unwrap(),
            name: Some("ignored".to_owned()),
            display_name: Some("Back Yard".to_owned()),
            manufacturer: Some("Hikvision".to_owned()),
            username: "operator".to_owned(),
            password: "new-secret".to_owned(),
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
            Some("rtsp://192.0.2.10/main")
        );
        assert_eq!(
            saved["cameras"]["existing"]["sub_rtsp_url"].as_str(),
            Some("rtsp://192.0.2.10/sub")
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
