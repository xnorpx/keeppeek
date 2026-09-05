mod broker;
pub mod config;
pub mod model;
mod outbox;
mod runtime;

pub use runtime::{Handle, MqttConnectionState, MqttStatus, Runtime};

use anyhow::Context as _;
use serde::Serialize;
use std::path::Path;

const LEGACY_OUTBOX_FILE: &str = "mqtt-forwarder.db";

pub fn remove_legacy_outbox(config_path: &Path) -> anyhow::Result<()> {
    let directory = config_path.parent().unwrap_or_else(|| Path::new("."));
    for suffix in ["", "-wal", "-shm"] {
        let path = directory.join(format!("{LEGACY_OUTBOX_FILE}{suffix}"));
        match std::fs::remove_file(&path) {
            Ok(()) => tracing::info!(path = %path.display(), "removed legacy MQTT outbox"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("unable to remove {}", path.display()));
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerFailureKind {
    Authentication,
    Tls,
    Network,
    Protocol,
    Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BrokerFailure {
    pub kind: BrokerFailureKind,
    pub detail: String,
}

impl std::fmt::Display for BrokerFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for BrokerFailure {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_outbox_cleanup_removes_database_family_and_is_idempotent() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-mqtt-outbox-cleanup-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let config_path = directory.join("config.toml");
        std::fs::write(&config_path, "").unwrap();
        for suffix in ["", "-wal", "-shm"] {
            std::fs::write(
                directory.join(format!("{LEGACY_OUTBOX_FILE}{suffix}")),
                "legacy",
            )
            .unwrap();
        }

        remove_legacy_outbox(&config_path).unwrap();
        remove_legacy_outbox(&config_path).unwrap();

        for suffix in ["", "-wal", "-shm"] {
            assert!(
                !directory
                    .join(format!("{LEGACY_OUTBOX_FILE}{suffix}"))
                    .exists()
            );
        }
        std::fs::remove_dir_all(directory).unwrap();
    }
}
