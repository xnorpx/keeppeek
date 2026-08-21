use crate::api::Health;
use clap::Parser;
use ureq::Agent;

#[derive(Parser, Debug)]
pub struct ClientCli {
    /// Server base URL
    #[arg(short, long, default_value = "http://localhost:3000")]
    pub server: String,
}

pub struct KeepPeekClient {
    http: Agent,
    base_url: String,
}

impl KeepPeekClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            http: Agent::config_builder().build().into(),
            base_url: base_url.trim_end_matches('/').to_owned(),
        }
    }

    pub fn health(&self) -> anyhow::Result<Health> {
        let response = self
            .http
            .get(&format!("{}/metrics", self.base_url))
            .call()?;
        if !response.status().is_success() {
            anyhow::bail!("GET /metrics returned HTTP {}", response.status());
        }
        Ok(Health {
            status: "ok".to_owned(),
        })
    }
}
