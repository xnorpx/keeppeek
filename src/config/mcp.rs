//! Configuration settings for the embedded KeepPeek MCP endpoint.
//!
//! All settings live under `[mcp]` in `config.toml`. The endpoint is disabled by
//! default and enforces explicit bounds on concurrency, payload sizes, timeouts,
//! and snapshot generation.

use serde::{Deserialize, Serialize};

/// Default maximum concurrent MCP requests.
pub const DEFAULT_MAX_CONCURRENT_REQUESTS: usize = 4;
/// Minimum allowed concurrent MCP requests.
pub const MIN_MAX_CONCURRENT_REQUESTS: usize = 1;
/// Maximum allowed concurrent MCP requests.
pub const MAX_MAX_CONCURRENT_REQUESTS: usize = 32;

/// Default maximum MCP JSON-RPC response size in bytes.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 65_536;
/// Minimum allowed MCP JSON-RPC response size in bytes.
pub const MIN_MAX_RESPONSE_BYTES: usize = 4_096;
/// Maximum allowed MCP JSON-RPC response size in bytes.
pub const MAX_MAX_RESPONSE_BYTES: usize = 262_144;

/// Default MCP request timeout in seconds.
pub const DEFAULT_REQUEST_TIMEOUT_SECONDS: u64 = 15;
/// Minimum allowed MCP request timeout in seconds.
pub const MIN_REQUEST_TIMEOUT_SECONDS: u64 = 1;
/// Maximum allowed MCP request timeout in seconds.
pub const MAX_REQUEST_TIMEOUT_SECONDS: u64 = 60;

/// Default maximum snapshot payload size in bytes.
pub const DEFAULT_MAX_SNAPSHOT_BYTES: usize = 262_144;
/// Minimum allowed snapshot payload size in bytes.
pub const MIN_MAX_SNAPSHOT_BYTES: usize = 16_384;
/// Maximum allowed snapshot payload size in bytes.
pub const MAX_MAX_SNAPSHOT_BYTES: usize = 1_048_576;

/// Default maximum snapshot image dimension in pixels.
pub const DEFAULT_MAX_SNAPSHOT_DIMENSION: u32 = 1280;
/// Minimum allowed snapshot image dimension in pixels.
pub const MIN_MAX_SNAPSHOT_DIMENSION: u32 = 160;
/// Maximum allowed snapshot image dimension in pixels.
pub const MAX_MAX_SNAPSHOT_DIMENSION: u32 = 1920;

/// Default minimum interval between snapshots per camera in seconds.
pub const DEFAULT_SNAPSHOT_MIN_INTERVAL_SECONDS: u64 = 5;
/// Minimum allowed interval between snapshots per camera in seconds.
pub const MIN_SNAPSHOT_MIN_INTERVAL_SECONDS: u64 = 1;
/// Maximum allowed interval between snapshots per camera in seconds.
pub const MAX_SNAPSHOT_MIN_INTERVAL_SECONDS: u64 = 300;

/// Default maximum byte budget for one camera debug capture session (64 MiB).
pub const DEFAULT_MAX_DEBUG_CAPTURE_BYTES: usize = 67_108_864;
/// Minimum allowed byte budget for one camera debug capture session (1 MiB).
pub const MIN_MAX_DEBUG_CAPTURE_BYTES: usize = 1_048_576;
/// Maximum allowed byte budget for one camera debug capture session (256 MiB).
pub const MAX_MAX_DEBUG_CAPTURE_BYTES: usize = 268_435_456;

/// Default maximum retained disk storage budget for camera debug captures in bytes (512 MiB).
pub const DEFAULT_MAX_DEBUG_STORAGE_BYTES: u64 = 536_870_912;
/// Minimum allowed retained disk storage budget for camera debug captures in bytes (16 MiB).
pub const MIN_MAX_DEBUG_STORAGE_BYTES: u64 = 16_777_216;
/// Maximum allowed retained disk storage budget for camera debug captures in bytes (4 GiB).
pub const MAX_MAX_DEBUG_STORAGE_BYTES: u64 = 4_294_967_296;

/// Maximum number of camera IDs permitted in `[mcp].debug_camera_ids`.
pub const MAX_DEBUG_CAMERA_IDS: usize = 256;
/// Maximum byte length of one entry in `[mcp].debug_camera_ids`.
pub const MAX_DEBUG_CAMERA_ID_BYTES: usize = 128;

const _: () = {
    assert!(MIN_MAX_DEBUG_CAPTURE_BYTES <= DEFAULT_MAX_DEBUG_CAPTURE_BYTES);
    assert!(DEFAULT_MAX_DEBUG_CAPTURE_BYTES <= MAX_MAX_DEBUG_CAPTURE_BYTES);
    assert!(MIN_MAX_DEBUG_STORAGE_BYTES <= DEFAULT_MAX_DEBUG_STORAGE_BYTES);
    assert!(DEFAULT_MAX_DEBUG_STORAGE_BYTES <= MAX_MAX_DEBUG_STORAGE_BYTES);
    assert!(MAX_DEBUG_CAMERA_IDS > 0);
    assert!(MAX_DEBUG_CAMERA_ID_BYTES > 0);
};

/// Embedded MCP server settings stored under `[mcp]` in `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpConfig {
    /// Enables the embedded `POST /mcp` endpoint when set to `true`.
    #[serde(default)]
    pub enabled: bool,
    /// Allows Administrator write tools through MCP when set to `true`.
    #[serde(default)]
    pub allow_writes: bool,
    /// Allows Administrator on-camera hardware device setting writes through MCP when set to `true`.
    #[serde(default)]
    pub allow_camera_device_writes: bool,
    /// Allows camera snapshot tools through MCP when set to `true`.
    #[serde(default)]
    pub allow_snapshots: bool,
    /// Allows raw Administrator camera debugging captures, transcripts, and credentials when `true`.
    #[serde(default)]
    pub allow_camera_debugging: bool,
    /// Maximum number of MCP requests that can execute at the same time.
    #[serde(default = "default_max_concurrent_requests")]
    pub max_concurrent_requests: usize,
    /// Maximum serialized JSON-RPC response payload size in bytes.
    #[serde(default = "default_max_response_bytes")]
    pub max_response_bytes: usize,
    /// Maximum execution time for one MCP request in seconds.
    #[serde(default = "default_request_timeout_seconds")]
    pub request_timeout_seconds: u64,
    /// Maximum encoded snapshot payload size in bytes.
    #[serde(default = "default_max_snapshot_bytes")]
    pub max_snapshot_bytes: usize,
    /// Maximum snapshot width or height in pixels.
    #[serde(default = "default_max_snapshot_dimension")]
    pub max_snapshot_dimension: u32,
    /// Minimum interval in seconds between consecutive snapshots for one camera.
    #[serde(default = "default_snapshot_min_interval_seconds")]
    pub snapshot_min_interval_seconds: u64,
    /// Maximum byte budget for one camera debug capture session.
    #[serde(default = "default_max_debug_capture_bytes")]
    pub max_debug_capture_bytes: usize,
    /// Maximum retained disk storage budget for camera debug captures in bytes.
    #[serde(default = "default_max_debug_storage_bytes")]
    pub max_debug_storage_bytes: u64,
    /// Optional allowlist of camera IDs permitted for MCP camera debugging (empty allows all).
    #[serde(default)]
    pub debug_camera_ids: Vec<String>,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_writes: false,
            allow_camera_device_writes: false,
            allow_snapshots: false,
            allow_camera_debugging: false,
            max_concurrent_requests: DEFAULT_MAX_CONCURRENT_REQUESTS,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            request_timeout_seconds: DEFAULT_REQUEST_TIMEOUT_SECONDS,
            max_snapshot_bytes: DEFAULT_MAX_SNAPSHOT_BYTES,
            max_snapshot_dimension: DEFAULT_MAX_SNAPSHOT_DIMENSION,
            snapshot_min_interval_seconds: DEFAULT_SNAPSHOT_MIN_INTERVAL_SECONDS,
            max_debug_capture_bytes: DEFAULT_MAX_DEBUG_CAPTURE_BYTES,
            max_debug_storage_bytes: DEFAULT_MAX_DEBUG_STORAGE_BYTES,
            debug_camera_ids: Vec::new(),
        }
    }
}

impl McpConfig {
    /// Returns `true` when every field equals its default value.
    #[must_use]
    pub const fn is_default(&self) -> bool {
        !self.enabled
            && !self.allow_writes
            && !self.allow_camera_device_writes
            && !self.allow_snapshots
            && !self.allow_camera_debugging
            && self.max_concurrent_requests == DEFAULT_MAX_CONCURRENT_REQUESTS
            && self.max_response_bytes == DEFAULT_MAX_RESPONSE_BYTES
            && self.request_timeout_seconds == DEFAULT_REQUEST_TIMEOUT_SECONDS
            && self.max_snapshot_bytes == DEFAULT_MAX_SNAPSHOT_BYTES
            && self.max_snapshot_dimension == DEFAULT_MAX_SNAPSHOT_DIMENSION
            && self.snapshot_min_interval_seconds == DEFAULT_SNAPSHOT_MIN_INTERVAL_SECONDS
            && self.max_debug_capture_bytes == DEFAULT_MAX_DEBUG_CAPTURE_BYTES
            && self.max_debug_storage_bytes == DEFAULT_MAX_DEBUG_STORAGE_BYTES
            && self.debug_camera_ids.is_empty()
    }

    /// Returns `true` when `camera_id` is allowed under `debug_camera_ids`.
    #[must_use]
    pub fn allows_debug_camera(&self, camera_id: &str) -> bool {
        self.debug_camera_ids.is_empty()
            || self
                .debug_camera_ids
                .iter()
                .any(|allowed| allowed == camera_id)
    }

    /// Validates all bounded numeric limits and camera allowlist entries in `[mcp]`.
    ///
    /// # Errors
    ///
    /// Returns an error when any numeric limit or camera allowlist entry is invalid.
    pub fn validate(&self) -> anyhow::Result<()> {
        validate_usize_range(
            "mcp.max_concurrent_requests",
            self.max_concurrent_requests,
            MIN_MAX_CONCURRENT_REQUESTS,
            MAX_MAX_CONCURRENT_REQUESTS,
        )?;
        validate_usize_range(
            "mcp.max_response_bytes",
            self.max_response_bytes,
            MIN_MAX_RESPONSE_BYTES,
            MAX_MAX_RESPONSE_BYTES,
        )?;
        validate_u64_range(
            "mcp.request_timeout_seconds",
            self.request_timeout_seconds,
            MIN_REQUEST_TIMEOUT_SECONDS,
            MAX_REQUEST_TIMEOUT_SECONDS,
        )?;
        validate_usize_range(
            "mcp.max_snapshot_bytes",
            self.max_snapshot_bytes,
            MIN_MAX_SNAPSHOT_BYTES,
            MAX_MAX_SNAPSHOT_BYTES,
        )?;
        validate_u32_range(
            "mcp.max_snapshot_dimension",
            self.max_snapshot_dimension,
            MIN_MAX_SNAPSHOT_DIMENSION,
            MAX_MAX_SNAPSHOT_DIMENSION,
        )?;
        validate_u64_range(
            "mcp.snapshot_min_interval_seconds",
            self.snapshot_min_interval_seconds,
            MIN_SNAPSHOT_MIN_INTERVAL_SECONDS,
            MAX_SNAPSHOT_MIN_INTERVAL_SECONDS,
        )?;
        validate_usize_range(
            "mcp.max_debug_capture_bytes",
            self.max_debug_capture_bytes,
            MIN_MAX_DEBUG_CAPTURE_BYTES,
            MAX_MAX_DEBUG_CAPTURE_BYTES,
        )?;
        validate_u64_range(
            "mcp.max_debug_storage_bytes",
            self.max_debug_storage_bytes,
            MIN_MAX_DEBUG_STORAGE_BYTES,
            MAX_MAX_DEBUG_STORAGE_BYTES,
        )?;
        validate_debug_camera_ids(&self.debug_camera_ids)?;
        Ok(())
    }
}

fn validate_debug_camera_ids(ids: &[String]) -> anyhow::Result<()> {
    anyhow::ensure!(
        ids.len() <= MAX_DEBUG_CAMERA_IDS,
        "mcp.debug_camera_ids must contain at most {MAX_DEBUG_CAMERA_IDS} entries (got {})",
        ids.len()
    );
    for (index, id) in ids.iter().enumerate() {
        anyhow::ensure!(
            !id.is_empty()
                && id.len() <= MAX_DEBUG_CAMERA_ID_BYTES
                && id.trim() == id
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii() && !byte.is_ascii_control()),
            "mcp.debug_camera_ids[{index}] must be a non-empty trimmed ASCII ID within {MAX_DEBUG_CAMERA_ID_BYTES} bytes"
        );
    }
    Ok(())
}

fn validate_usize_range(name: &str, value: usize, min: usize, max: usize) -> anyhow::Result<()> {
    anyhow::ensure!(
        (min..=max).contains(&value),
        "{name} must be between {min} and {max} (got {value})"
    );
    Ok(())
}

fn validate_u64_range(name: &str, value: u64, min: u64, max: u64) -> anyhow::Result<()> {
    anyhow::ensure!(
        (min..=max).contains(&value),
        "{name} must be between {min} and {max} (got {value})"
    );
    Ok(())
}

fn validate_u32_range(name: &str, value: u32, min: u32, max: u32) -> anyhow::Result<()> {
    anyhow::ensure!(
        (min..=max).contains(&value),
        "{name} must be between {min} and {max} (got {value})"
    );
    Ok(())
}

const fn default_max_concurrent_requests() -> usize {
    DEFAULT_MAX_CONCURRENT_REQUESTS
}

const fn default_max_response_bytes() -> usize {
    DEFAULT_MAX_RESPONSE_BYTES
}

const fn default_request_timeout_seconds() -> u64 {
    DEFAULT_REQUEST_TIMEOUT_SECONDS
}

const fn default_max_snapshot_bytes() -> usize {
    DEFAULT_MAX_SNAPSHOT_BYTES
}

const fn default_max_snapshot_dimension() -> u32 {
    DEFAULT_MAX_SNAPSHOT_DIMENSION
}

const fn default_snapshot_min_interval_seconds() -> u64 {
    DEFAULT_SNAPSHOT_MIN_INTERVAL_SECONDS
}

const fn default_max_debug_capture_bytes() -> usize {
    DEFAULT_MAX_DEBUG_CAPTURE_BYTES
}

const fn default_max_debug_storage_bytes() -> u64 {
    DEFAULT_MAX_DEBUG_STORAGE_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_disabled_and_valid() {
        let config = McpConfig::default();
        assert!(!config.enabled);
        assert!(!config.allow_writes);
        assert!(!config.allow_camera_device_writes);
        assert!(!config.allow_snapshots);
        assert!(!config.allow_camera_debugging);
        assert_eq!(
            config.max_debug_capture_bytes,
            DEFAULT_MAX_DEBUG_CAPTURE_BYTES
        );
        assert_eq!(
            config.max_debug_storage_bytes,
            DEFAULT_MAX_DEBUG_STORAGE_BYTES
        );
        assert!(config.debug_camera_ids.is_empty());
        assert!(config.is_default());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validation_accepts_exact_lower_and_upper_bounds() {
        let valid_min = McpConfig {
            enabled: true,
            allow_writes: true,
            allow_camera_device_writes: true,
            allow_snapshots: true,
            allow_camera_debugging: true,
            max_concurrent_requests: MIN_MAX_CONCURRENT_REQUESTS,
            max_response_bytes: MIN_MAX_RESPONSE_BYTES,
            request_timeout_seconds: MIN_REQUEST_TIMEOUT_SECONDS,
            max_snapshot_bytes: MIN_MAX_SNAPSHOT_BYTES,
            max_snapshot_dimension: MIN_MAX_SNAPSHOT_DIMENSION,
            snapshot_min_interval_seconds: MIN_SNAPSHOT_MIN_INTERVAL_SECONDS,
            max_debug_capture_bytes: MIN_MAX_DEBUG_CAPTURE_BYTES,
            max_debug_storage_bytes: MIN_MAX_DEBUG_STORAGE_BYTES,
            debug_camera_ids: vec!["192.0.2.10".to_owned()],
        };
        assert!(valid_min.validate().is_ok());
        assert!(!valid_min.is_default());

        let valid_max = McpConfig {
            enabled: true,
            allow_writes: false,
            allow_camera_device_writes: false,
            allow_snapshots: false,
            allow_camera_debugging: false,
            max_concurrent_requests: MAX_MAX_CONCURRENT_REQUESTS,
            max_response_bytes: MAX_MAX_RESPONSE_BYTES,
            request_timeout_seconds: MAX_REQUEST_TIMEOUT_SECONDS,
            max_snapshot_bytes: MAX_MAX_SNAPSHOT_BYTES,
            max_snapshot_dimension: MAX_MAX_SNAPSHOT_DIMENSION,
            snapshot_min_interval_seconds: MAX_SNAPSHOT_MIN_INTERVAL_SECONDS,
            max_debug_capture_bytes: MAX_MAX_DEBUG_CAPTURE_BYTES,
            max_debug_storage_bytes: MAX_MAX_DEBUG_STORAGE_BYTES,
            debug_camera_ids: (0..MAX_DEBUG_CAMERA_IDS)
                .map(|index| format!("cam-{index}"))
                .collect(),
        };
        assert!(valid_max.validate().is_ok());
    }

    #[test]
    fn validation_rejects_out_of_range_numeric_bounds() {
        let invalid_cases = [
            McpConfig {
                max_concurrent_requests: 0,
                ..McpConfig::default()
            },
            McpConfig {
                max_concurrent_requests: MAX_MAX_CONCURRENT_REQUESTS + 1,
                ..McpConfig::default()
            },
            McpConfig {
                max_response_bytes: MIN_MAX_RESPONSE_BYTES - 1,
                ..McpConfig::default()
            },
            McpConfig {
                max_response_bytes: MAX_MAX_RESPONSE_BYTES + 1,
                ..McpConfig::default()
            },
            McpConfig {
                request_timeout_seconds: 0,
                ..McpConfig::default()
            },
            McpConfig {
                request_timeout_seconds: MAX_REQUEST_TIMEOUT_SECONDS + 1,
                ..McpConfig::default()
            },
            McpConfig {
                max_snapshot_bytes: MIN_MAX_SNAPSHOT_BYTES - 1,
                ..McpConfig::default()
            },
            McpConfig {
                max_snapshot_bytes: MAX_MAX_SNAPSHOT_BYTES + 1,
                ..McpConfig::default()
            },
            McpConfig {
                max_snapshot_dimension: MIN_MAX_SNAPSHOT_DIMENSION - 1,
                ..McpConfig::default()
            },
            McpConfig {
                max_snapshot_dimension: MAX_MAX_SNAPSHOT_DIMENSION + 1,
                ..McpConfig::default()
            },
            McpConfig {
                snapshot_min_interval_seconds: 0,
                ..McpConfig::default()
            },
            McpConfig {
                snapshot_min_interval_seconds: MAX_SNAPSHOT_MIN_INTERVAL_SECONDS + 1,
                ..McpConfig::default()
            },
        ];

        for invalid in invalid_cases {
            assert!(invalid.validate().is_err());
        }
    }

    #[test]
    fn validation_rejects_invalid_debug_bounds_and_camera_ids() {
        let invalid_cases = [
            McpConfig {
                max_debug_capture_bytes: MIN_MAX_DEBUG_CAPTURE_BYTES - 1,
                ..McpConfig::default()
            },
            McpConfig {
                max_debug_capture_bytes: MAX_MAX_DEBUG_CAPTURE_BYTES + 1,
                ..McpConfig::default()
            },
            McpConfig {
                max_debug_storage_bytes: MIN_MAX_DEBUG_STORAGE_BYTES - 1,
                ..McpConfig::default()
            },
            McpConfig {
                max_debug_storage_bytes: MAX_MAX_DEBUG_STORAGE_BYTES + 1,
                ..McpConfig::default()
            },
            McpConfig {
                debug_camera_ids: vec![String::new()],
                ..McpConfig::default()
            },
            McpConfig {
                debug_camera_ids: vec![" 192.0.2.10".to_owned()],
                ..McpConfig::default()
            },
            McpConfig {
                debug_camera_ids: vec!["cam\n1".to_owned()],
                ..McpConfig::default()
            },
            McpConfig {
                debug_camera_ids: vec!["cám".to_owned()],
                ..McpConfig::default()
            },
            McpConfig {
                debug_camera_ids: vec!["x".repeat(MAX_DEBUG_CAMERA_ID_BYTES + 1)],
                ..McpConfig::default()
            },
            McpConfig {
                debug_camera_ids: (0..=MAX_DEBUG_CAMERA_IDS)
                    .map(|index| format!("cam-{index}"))
                    .collect(),
                ..McpConfig::default()
            },
        ];

        for invalid in invalid_cases {
            assert!(invalid.validate().is_err());
        }
    }

    #[test]
    fn toml_round_trip_omits_default_and_preserves_unrelated_sections() {
        let raw = r#"
host = "127.0.0.1"
port = 8081

[custom_section]
keep_me = "yes"

[mcp]
enabled = true
max_concurrent_requests = 8
allow_camera_debugging = true
debug_camera_ids = ["192.0.2.10"]
"#;
        let table: toml::Table = toml::from_str(raw).unwrap();
        let config = crate::config::config_from_table_for_test(&table).unwrap();
        assert!(config.mcp.enabled);
        assert!(config.mcp.allow_camera_debugging);
        assert_eq!(config.mcp.max_concurrent_requests, 8);
        assert_eq!(
            config.mcp.max_debug_capture_bytes,
            DEFAULT_MAX_DEBUG_CAPTURE_BYTES
        );
        assert_eq!(
            config.mcp.max_debug_storage_bytes,
            DEFAULT_MAX_DEBUG_STORAGE_BYTES
        );
        assert_eq!(config.mcp.debug_camera_ids, vec!["192.0.2.10"]);
        assert!(!config.mcp.allow_writes);

        let default_serialized = toml::to_string(&crate::config::Config::default()).unwrap();
        assert!(!default_serialized.contains("[mcp]"));
        let non_default_serialized = toml::to_string(&config).unwrap();
        assert!(non_default_serialized.contains("allow_camera_debugging = true"));
        assert!(non_default_serialized.contains("debug_camera_ids = [\"192.0.2.10\"]"));
    }

    #[test]
    fn allows_debug_camera_checks_empty_and_scoped_lists() {
        let mut config = McpConfig::default();
        assert!(config.allows_debug_camera("192.0.2.10"));
        config.debug_camera_ids = vec!["192.0.2.10".to_owned()];
        assert!(config.allows_debug_camera("192.0.2.10"));
        assert!(!config.allows_debug_camera("192.0.2.11"));
    }

    #[test]
    fn unknown_mcp_field_is_rejected() {
        let raw = r#"
[mcp]
enabled = true
unknown_flag = true
"#;
        let table: toml::Table = toml::from_str(raw).unwrap();
        assert!(crate::config::config_from_table_for_test(&table).is_err());
    }
}
