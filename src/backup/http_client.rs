use crate::api::backup_proto;
use serde::de::DeserializeOwned;
use std::{fmt, fs::File, path::Path, time::Duration};
use ureq::{Agent, Body, http::Response};
use url::{Host, Url};

const CONTROL_BODY_BYTES_MAX: u64 = 16 * 1024 * 1024;
const ERROR_BODY_BYTES_MAX: u64 = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug)]
pub enum BackupClientError {
    Transport,
    Protocol(&'static str),
    Api {
        status: u16,
        error: backup_proto::BackupError,
    },
}

impl BackupClientError {
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Api { status, .. } if *status < 500 => 3,
            Self::Transport | Self::Protocol(_) | Self::Api { .. } => 4,
        }
    }
}

impl fmt::Display for BackupClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport => formatter.write_str("backup server request failed"),
            Self::Protocol(message) => formatter.write_str(message),
            Self::Api { status, error } => write!(
                formatter,
                "backup server returned HTTP {status}: {}",
                error.message
            ),
        }
    }
}

impl std::error::Error for BackupClientError {}

pub struct BackupHttpClient {
    agent: Agent,
    base_url: Url,
    authorization: Option<String>,
}

impl BackupHttpClient {
    /// Creates a client. Bearer credentials are refused over non-loopback HTTP.
    pub fn new(base_url: &str, access_key: Option<String>) -> Result<Self, BackupClientError> {
        let mut base_url = Url::parse(base_url)
            .map_err(|_| BackupClientError::Protocol("backup server URL is invalid"))?;
        if !matches!(base_url.scheme(), "http" | "https")
            || base_url.cannot_be_a_base()
            || !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
        {
            return Err(BackupClientError::Protocol(
                "backup server URL must be an HTTP(S) origin without credentials",
            ));
        }
        if access_key.is_some() && base_url.scheme() == "http" && !is_loopback(&base_url) {
            return Err(BackupClientError::Protocol(
                "an access key requires HTTPS unless the server is loopback",
            ));
        }
        let base_path = base_url.path().trim_end_matches('/').to_owned();
        base_url.set_path(&base_path);
        let agent = Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(REQUEST_TIMEOUT))
            .build()
            .into();
        Ok(Self {
            agent,
            base_url,
            authorization: access_key.map(|key| format!("Bearer {key}")),
        })
    }

    pub fn apply(&self, path: &Path) -> Result<backup_proto::RestoreRecord, BackupClientError> {
        let file = File::open(path)
            .map_err(|_| BackupClientError::Protocol("configuration archive is unavailable"))?;
        let content_length = file
            .metadata()
            .map_err(|_| BackupClientError::Protocol("configuration archive is unavailable"))?
            .len();
        if content_length == 0
            || content_length > super::DEFAULT_INSPECTION_LIMITS.maximum_archive_bytes
        {
            return Err(BackupClientError::Protocol(
                "configuration archive is outside the supported size limit",
            ));
        }
        let url = self.url("/config/apply")?;
        let mut request = self
            .agent
            .post(url.as_str())
            .content_type("application/zip")
            .header("Accept", "application/json")
            .header("Content-Length", content_length.to_string());
        if let Some(authorization) = &self.authorization {
            request = request.header("Authorization", authorization);
        }
        let response = request
            .send(file)
            .map_err(|_| BackupClientError::Transport)?;
        let record: backup_proto::RestoreRecord = decode_json_response(response)?;
        if record.state != backup_proto::RestoreState::AwaitingRestart as i32 {
            return Err(BackupClientError::Protocol(
                "configuration apply did not return a staged restore",
            ));
        }
        Ok(record)
    }

    pub fn export(&self, destination: &Path) -> Result<u64, BackupClientError> {
        let url = self.url("/config/export")?;
        let mut request = self
            .agent
            .get(url.as_str())
            .header("Accept", "application/zip");
        if let Some(authorization) = &self.authorization {
            request = request.header("Authorization", authorization);
        }
        let response = request.call().map_err(|_| BackupClientError::Transport)?;
        if !response.status().is_success() {
            return Err(decode_api_error(response));
        }
        if response
            .headers()
            .get("Content-Type")
            .and_then(|value| value.to_str().ok())
            != Some("application/zip")
        {
            return Err(BackupClientError::Protocol(
                "configuration export did not return a ZIP archive",
            ));
        }
        let mut output = create_private_file(destination)?;
        let result = save_export(response, &mut output);
        drop(output);
        if result.is_err() && std::fs::remove_file(destination).is_err() {
            return Err(BackupClientError::Protocol(
                "incomplete configuration export could not be removed",
            ));
        }
        result
    }

    fn url(&self, path: &str) -> Result<Url, BackupClientError> {
        self.base_url
            .join(path)
            .map_err(|_| BackupClientError::Protocol("backup endpoint URL is invalid"))
    }
}

fn save_export(response: Response<Body>, output: &mut File) -> Result<u64, BackupClientError> {
    let maximum_bytes = super::DEFAULT_INSPECTION_LIMITS.maximum_archive_bytes;
    let copied = std::io::copy(
        &mut response
            .into_body()
            .into_with_config()
            .limit(maximum_bytes + 1)
            .reader(),
        output,
    )
    .map_err(|_| BackupClientError::Transport)?;
    if copied == 0 || copied > maximum_bytes {
        return Err(BackupClientError::Protocol(
            "configuration export is outside the supported size limit",
        ));
    }
    output
        .sync_all()
        .map_err(|_| BackupClientError::Protocol("configuration export could not be saved"))?;
    Ok(copied)
}

fn decode_json_response<Output: DeserializeOwned>(
    mut response: Response<Body>,
) -> Result<Output, BackupClientError> {
    if !response.status().is_success() {
        return Err(decode_api_error(response));
    }
    let body = response
        .body_mut()
        .with_config()
        .limit(CONTROL_BODY_BYTES_MAX)
        .read_to_string()
        .map_err(|_| BackupClientError::Protocol("backup response exceeded its size limit"))?;
    serde_json::from_str(&body)
        .map_err(|_| BackupClientError::Protocol("backup server returned invalid ProtoJSON"))
}

fn decode_api_error(mut response: Response<Body>) -> BackupClientError {
    let status = response.status().as_u16();
    let error = response
        .body_mut()
        .with_config()
        .limit(ERROR_BODY_BYTES_MAX)
        .read_to_string()
        .ok()
        .and_then(|body| serde_json::from_str(&body).ok())
        .unwrap_or_else(|| backup_proto::BackupError {
            code: backup_proto::BackupErrorCode::Internal as i32,
            message: "backup request was rejected".to_owned(),
            field: String::new(),
            retryable: status >= 500,
        });
    BackupClientError::Api { status, error }
}

fn create_private_file(path: &Path) -> Result<File, BackupClientError> {
    let mut options = File::options();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|_| BackupClientError::Protocol("backup destination could not be created"))
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}
