//! File-only camera event policy and camera-bound endpoint validation.
//!
//! Configure this policy in `[cameras.<name>.events]`. The fixed protobuf camera
//! settings contract does not expose these fields. Validation does not contact
//! cameras or perform DNS lookup.
//!
//! Topic filters use a namespace-expanded root and a slash-separated local path,
//! such as `{http://www.onvif.org/ver10/topics}VideoSource/MotionAlarm`. Document
//! prefixes such as `tns1:VideoSource/MotionAlarm` are not stable topic identities.
//! Validation accepts at most 32 source tokens of 256 UTF-8 bytes each and 32
//! combined include/exclude filters of 1024 UTF-8 bytes each. Entries must not be
//! empty or contain control characters. Source tokens retain their exact text.

use std::{fmt, net::IpAddr};

use anyhow::Context as _;
use onvif::event::Endpoint;
use serde::{Deserialize, Serialize};
use url::{Host, Url};

const SOURCE_TOKEN_COUNT_MAX: usize = 32;
const SOURCE_TOKEN_SIZE_BYTES_MAX: usize = 256;
const TOPIC_FILTER_COUNT_MAX: usize = 32;
const TOPIC_FILTER_SIZE_BYTES_MAX: usize = 1024;

/// Per-camera event selection independent of the video transport backend.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EventConfig {
    pub mode: EventMode,
    pub metadata_stream: MetadataMode,
    /// An explicit HTTP(S) endpoint on the configured camera IP.
    pub event_service_url: Option<String>,
    /// Exact source tokens that map notifications to this camera or channel.
    pub source_tokens: Vec<String>,
    /// Topic paths with a namespace-expanded root, never document prefixes.
    pub include_topics: Vec<String>,
    /// Topic paths with a namespace-expanded root, never document prefixes.
    pub exclude_topics: Vec<String>,
    pub snapshots: bool,
}

/// The requested event transport, independent of video stream selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventMode {
    #[default]
    Auto,
    Vendor,
    OnvifPullpoint,
    RtspMetadata,
    Disabled,
}

/// Whether the camera's RTSP metadata stream may be used.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MetadataMode {
    #[default]
    Auto,
    Enabled,
    Disabled,
}

impl Default for EventConfig {
    fn default() -> Self {
        Self {
            mode: EventMode::default(),
            metadata_stream: MetadataMode::default(),
            event_service_url: None,
            source_tokens: Vec::new(),
            include_topics: Vec::new(),
            exclude_topics: Vec::new(),
            snapshots: true,
        }
    }
}

impl fmt::Debug for EventConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EventConfig")
            .field("mode", &self.mode)
            .field("metadata_stream", &self.metadata_stream)
            .field(
                "event_service_url_configured",
                &self.event_service_url.is_some(),
            )
            .field("source_token_count", &self.source_tokens.len())
            .field("include_topic_count", &self.include_topics.len())
            .field("exclude_topic_count", &self.exclude_topics.len())
            .field("snapshots", &self.snapshots)
            .finish()
    }
}

impl EventConfig {
    /// Identifies policies omitted from legacy camera configuration writes.
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }

    /// Validates an event policy without contacting the configured camera.
    /// Resolve secret references before calling this method.
    ///
    /// # Errors
    /// Rejects unsafe or foreign endpoints, excess counts or bytes, empty entries,
    /// control characters, and topic paths without namespace-expanded roots.
    pub fn validate(&self, camera_ip: IpAddr) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.source_tokens.len() <= SOURCE_TOKEN_COUNT_MAX,
            "event source token count exceeds {SOURCE_TOKEN_COUNT_MAX}"
        );
        let filter_count = self
            .include_topics
            .len()
            .checked_add(self.exclude_topics.len())
            .context("event topic filter count overflow")?;
        anyhow::ensure!(
            filter_count <= TOPIC_FILTER_COUNT_MAX,
            "event topic filter count exceeds {TOPIC_FILTER_COUNT_MAX}"
        );
        for token in &self.source_tokens {
            validate_entry(token, SOURCE_TOKEN_SIZE_BYTES_MAX, "event source token")?;
        }
        for topic in self.include_topics.iter().chain(&self.exclude_topics) {
            validate_entry(topic, TOPIC_FILTER_SIZE_BYTES_MAX, "event topic filter")?;
            validate_topic_filter(topic)?;
        }
        if let Some(configured) = &self.event_service_url {
            let endpoint = Endpoint::new(configured).context("invalid event service URL")?;
            let url = Url::parse(endpoint.as_str()).context("invalid event service URL")?;
            let endpoint_ip = match url.host() {
                Some(Host::Ipv4(ip)) => Some(IpAddr::V4(ip)),
                Some(Host::Ipv6(ip)) => Some(IpAddr::V6(ip)),
                _ => None,
            };
            anyhow::ensure!(
                endpoint_ip == Some(camera_ip),
                "event service URL must use the configured camera IP"
            );
        }
        Ok(())
    }
}

fn validate_entry(value: &str, size_bytes_max: usize, kind: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        value.len() <= size_bytes_max,
        "{kind} exceeds the {size_bytes_max}-byte limit"
    );
    anyhow::ensure!(!value.trim().is_empty(), "{kind} must not be empty");
    anyhow::ensure!(
        !value.chars().any(char::is_control),
        "{kind} must not contain control characters"
    );
    Ok(())
}

fn validate_topic_filter(topic: &str) -> anyhow::Result<()> {
    let (namespace, path) = topic
        .strip_prefix('{')
        .and_then(|expanded| expanded.split_once('}'))
        .context("event topic filters must use a namespace-expanded root")?;
    anyhow::ensure!(
        !namespace.is_empty()
            && !namespace.contains(['{', '}'])
            && !namespace.chars().any(char::is_whitespace),
        "event topic filters must contain a nonempty namespace URI"
    );
    anyhow::ensure!(
        path.split('/').all(|segment| {
            !segment.is_empty()
                && !segment.contains([':', '{', '}'])
                && !segment.chars().any(char::is_whitespace)
        }),
        "event topic filters must contain a nonempty path without namespace prefixes"
    );
    Ok(())
}
