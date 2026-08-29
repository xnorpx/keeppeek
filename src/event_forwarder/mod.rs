mod broker;
pub mod config;
pub mod model;
mod outbox;
mod runtime;

pub use runtime::{Handle, MqttConnectionState, MqttStatus, Runtime};

use serde::Serialize;

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
