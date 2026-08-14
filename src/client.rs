use crate::api::{CameraId, CameraStatus, Health, Ready};
use clap::Parser;
use serde::de::DeserializeOwned;
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
        self.get_json("/health")
    }

    pub fn ready(&self) -> anyhow::Result<Ready> {
        self.get_json("/ready")
    }

    pub fn cameras(&self) -> anyhow::Result<Vec<CameraStatus>> {
        self.get_json("/api/v1/cameras")
    }

    pub fn camera(&self, id: &CameraId) -> anyhow::Result<CameraStatus> {
        self.get_json(&format!("/api/v1/cameras/{id}"))
    }

    fn get_json<T: DeserializeOwned>(&self, path: &str) -> anyhow::Result<T> {
        let response = self.http.get(&format!("{}{path}", self.base_url)).call()?;
        if !response.status().is_success() {
            anyhow::bail!("GET {path} returned HTTP {}", response.status());
        }
        let body = response.into_body().read_to_string()?;
        Ok(serde_json::from_str(&body)?)
    }
}
