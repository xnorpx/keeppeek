use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use url::Url;

const DEFAULT_OUTBOX_MAX_MB: u64 = 64;
const DEFAULT_RETRY_MIN_MS: u64 = 250;
const DEFAULT_RETRY_MAX_MS: u64 = 30_000;
pub const MQTT_PASSWORD_SECRET: &str = "MQTT_PASSWORD";

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct EventForwarderConfig {
    pub mqtt: MqttForwarderConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct MqttForwarderConfig {
    #[serde(default = "default_configuration_revision")]
    pub revision: u64,
    pub enabled: bool,
    pub broker_url: String,
    pub client_id: String,
    pub instance_id: String,
    pub forwarder_id: String,
    pub topic_prefix: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub tls_ca_path: Option<PathBuf>,
    pub qos: u8,
    pub retain_events: bool,
    pub retain_health: bool,
    pub outbox_max_mb: u64,
    pub retry_min_ms: u64,
    pub retry_max_ms: u64,
}

impl MqttForwarderConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        let broker_url = Url::parse(&self.broker_url)
            .map_err(|error| anyhow::anyhow!("MQTT broker URL is invalid: {error}"))?;
        if !matches!(broker_url.scheme(), "mqtt" | "mqtts") {
            anyhow::bail!("MQTT broker URL scheme must be mqtt or mqtts");
        }
        if broker_url.host_str().is_none() {
            anyhow::bail!("MQTT broker URL must include a host");
        }
        if !broker_url.username().is_empty() || broker_url.password().is_some() {
            anyhow::bail!("MQTT credentials must use the username and password fields");
        }
        if broker_url.path() != "" && broker_url.path() != "/" {
            anyhow::bail!("MQTT broker URL cannot include a path");
        }
        if broker_url.query().is_some() || broker_url.fragment().is_some() {
            anyhow::bail!("MQTT broker URL cannot include a query or fragment");
        }
        validate_identifier(&self.client_id, "MQTT client ID")?;
        validate_identifier(&self.instance_id, "MQTT instance ID")?;
        validate_identifier(&self.forwarder_id, "MQTT forwarder ID")?;
        if self.topic_prefix.is_empty()
            || self.topic_prefix.len() > 512
            || self.topic_prefix.starts_with('/')
            || self.topic_prefix.ends_with('/')
            || self.topic_prefix.contains(['\0', '+', '#'])
        {
            anyhow::bail!(
                "MQTT topic prefix must contain 1 to 512 bytes without leading or trailing slashes, NUL, +, or #"
            );
        }
        if self.qos > 2 {
            anyhow::bail!("MQTT QoS must be 0, 1, or 2");
        }
        if self.outbox_max_mb == 0 || self.outbox_max_mb > 65_536 {
            anyhow::bail!("MQTT outbox limit must be between 1 and 65536 MiB");
        }
        if self.retry_min_ms == 0 || self.retry_min_ms > self.retry_max_ms {
            anyhow::bail!("MQTT retry minimum must be nonzero and cannot exceed its maximum");
        }
        if self.retry_max_ms > 3_600_000 {
            anyhow::bail!("MQTT retry maximum cannot exceed one hour");
        }
        if broker_url.scheme() == "mqtt" && self.tls_ca_path.is_some() {
            anyhow::bail!("MQTT TLS trust can only be configured for an mqtts broker URL");
        }
        if self.password.is_some() && self.username.is_none() {
            anyhow::bail!("MQTT password requires a username");
        }
        Ok(())
    }
}

impl Default for MqttForwarderConfig {
    fn default() -> Self {
        Self {
            revision: default_configuration_revision(),
            enabled: false,
            broker_url: "mqtt://127.0.0.1:1883".to_owned(),
            client_id: "keeppeek".to_owned(),
            instance_id: "home-nvr".to_owned(),
            forwarder_id: "mqtt".to_owned(),
            topic_prefix: "keeppeek".to_owned(),
            username: None,
            password: None,
            tls_ca_path: None,
            qos: 1,
            retain_events: false,
            retain_health: true,
            outbox_max_mb: DEFAULT_OUTBOX_MAX_MB,
            retry_min_ms: DEFAULT_RETRY_MIN_MS,
            retry_max_ms: DEFAULT_RETRY_MAX_MS,
        }
    }
}

const fn default_configuration_revision() -> u64 {
    1
}

fn validate_identifier(value: &str, label: &str) -> anyhow::Result<()> {
    if value.is_empty() || value.len() > 128 || value.contains(['\0', '/', '+', '#']) {
        anyhow::bail!("{label} must contain 1 to 128 bytes without NUL, /, +, or #");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_local_configuration() {
        let config = MqttForwarderConfig::default();
        config.validate().unwrap();
    }

    #[test]
    fn accepts_authenticated_tls_configuration() {
        let config = MqttForwarderConfig {
            broker_url: "mqtts://broker.home.example:8883".to_owned(),
            username: Some("keeppeek".to_owned()),
            password: Some("{secret:MQTT_PASSWORD}".to_owned()),
            tls_ca_path: Some(PathBuf::from("/etc/keeppeek/mqtt-ca.pem")),
            ..MqttForwarderConfig::default()
        };
        config.validate().unwrap();
    }

    #[test]
    fn rejects_credentials_in_broker_url() {
        let config = MqttForwarderConfig {
            broker_url: "mqtts://operator:secret@broker.example:8883".to_owned(),
            ..MqttForwarderConfig::default()
        };
        assert_eq!(
            config.validate().unwrap_err().to_string(),
            "MQTT credentials must use the username and password fields"
        );
    }

    #[test]
    fn rejects_topic_wildcards() {
        let config = MqttForwarderConfig {
            topic_prefix: "keeppeek/+/events".to_owned(),
            ..MqttForwarderConfig::default()
        };
        assert!(config.validate().is_err());
    }
}
