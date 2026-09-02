use crate::api::backup_proto;
use serde::{Serialize, de::DeserializeOwned};
use std::{fmt, fs::File, io::Write as _, path::Path, time::Duration};
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
            .timeout_global(Some(REQUEST_TIMEOUT))
            .build()
            .into();
        Ok(Self {
            agent,
            base_url,
            authorization: access_key.map(|key| format!("Bearer {key}")),
        })
    }

    pub fn capabilities(&self) -> Result<backup_proto::BackupCapabilities, BackupClientError> {
        self.get_json("/api/backups/capabilities")
    }

    pub fn list(&self) -> Result<backup_proto::ListBackupsResponse, BackupClientError> {
        self.get_json("/api/backups")
    }

    pub fn create(
        &self,
        request: &backup_proto::CreateBackupRequest,
    ) -> Result<backup_proto::BackupRecord, BackupClientError> {
        self.post_json("/api/backups", request)
    }

    pub fn inspect(
        &self,
        backup_id: &str,
    ) -> Result<backup_proto::BackupRecord, BackupClientError> {
        self.post_json(
            "/api/backups/inspect",
            &backup_proto::InspectBackupRequest {
                backup_id: backup_id.to_owned(),
            },
        )
    }

    pub fn create_restore_plan(
        &self,
        request: &backup_proto::CreateRestorePlanRequest,
    ) -> Result<backup_proto::RestorePlan, BackupClientError> {
        self.post_json("/api/backups/restore-plans", request)
    }

    pub fn activate(
        &self,
        request: &backup_proto::ActivateRestoreRequest,
    ) -> Result<backup_proto::RestoreRecord, BackupClientError> {
        self.post_json("/api/backups/restores", request)
    }

    pub fn get_restore(
        &self,
        restore_id: &str,
    ) -> Result<backup_proto::RestoreRecord, BackupClientError> {
        self.post_json(
            "/api/backups/restores/get",
            &backup_proto::GetRestoreRequest {
                restore_id: restore_id.to_owned(),
            },
        )
    }

    pub fn rollback(
        &self,
        request: &backup_proto::RollbackRestoreRequest,
    ) -> Result<backup_proto::RestoreRecord, BackupClientError> {
        self.post_json("/api/backups/rollbacks", request)
    }

    pub fn delete(
        &self,
        request: &backup_proto::DeleteBackupRequest,
    ) -> Result<backup_proto::DeleteBackupResponse, BackupClientError> {
        self.post_json("/api/backups/delete", request)
    }

    pub fn upload(&self, path: &Path) -> Result<backup_proto::BackupRecord, BackupClientError> {
        let file_name = path.file_name().and_then(|value| value.to_str()).ok_or(
            BackupClientError::Protocol("backup upload file name is invalid"),
        )?;
        let content_length = std::fs::metadata(path)
            .map_err(|_| BackupClientError::Protocol("backup upload file is unavailable"))?
            .len();
        let transfer: backup_proto::BackupTransfer = self.post_json(
            "/api/backups/uploads",
            &backup_proto::BeginBackupUploadRequest {
                client_request_id: uuid::Uuid::new_v4().to_string(),
                file_name: file_name.to_owned(),
                content_length,
                archive_sha256: None,
            },
        )?;
        if transfer.uri != "/api/backups/transfers" || content_length > transfer.maximum_bytes {
            return Err(BackupClientError::Protocol(
                "backup server returned an invalid upload transfer",
            ));
        }
        let mut url = self.url(&transfer.uri)?;
        url.query_pairs_mut()
            .append_pair("transfer_id", &transfer.transfer_id);
        let file = File::open(path)
            .map_err(|_| BackupClientError::Protocol("backup upload file is unavailable"))?;
        let mut request = self.agent.put(url.as_str()).content_type("application/zip");
        if let Some(authorization) = &self.authorization {
            request = request.header("Authorization", authorization);
        }
        let response = request
            .send(file)
            .map_err(|_| BackupClientError::Transport)?;
        decode_json_response(response)
    }

    pub fn download(
        &self,
        backup_id: &str,
        destination: &Path,
        maximum_bytes: u64,
    ) -> Result<u64, BackupClientError> {
        let mut url = self.url("/api/backups/download")?;
        url.query_pairs_mut().append_pair("backup_id", backup_id);
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
        let mut output = create_private_file(destination)?;
        let copied = std::io::copy(
            &mut response
                .into_body()
                .into_with_config()
                .limit(maximum_bytes.saturating_add(1))
                .reader(),
            &mut output,
        )
        .map_err(|_| BackupClientError::Transport)?;
        if copied == 0 || copied > maximum_bytes {
            drop(output);
            let _ = std::fs::remove_file(destination);
            return Err(BackupClientError::Protocol(
                "backup download is outside the declared size limit",
            ));
        }
        output
            .flush()
            .map_err(|_| BackupClientError::Protocol("backup download could not be saved"))?;
        Ok(copied)
    }

    fn get_json<Output: DeserializeOwned>(&self, path: &str) -> Result<Output, BackupClientError> {
        let url = self.url(path)?;
        let mut request = self
            .agent
            .get(url.as_str())
            .header("Accept", "application/json");
        if let Some(authorization) = &self.authorization {
            request = request.header("Authorization", authorization);
        }
        let response = request.call().map_err(|_| BackupClientError::Transport)?;
        decode_json_response(response)
    }

    fn post_json<Input: Serialize, Output: DeserializeOwned>(
        &self,
        path: &str,
        input: &Input,
    ) -> Result<Output, BackupClientError> {
        let url = self.url(path)?;
        let body = serde_json::to_vec(input)
            .map_err(|_| BackupClientError::Protocol("backup request could not be encoded"))?;
        let mut request = self
            .agent
            .post(url.as_str())
            .content_type("application/json")
            .header("Accept", "application/json");
        if let Some(authorization) = &self.authorization {
            request = request.header("Authorization", authorization);
        }
        let response = request
            .send(body)
            .map_err(|_| BackupClientError::Transport)?;
        decode_json_response(response)
    }

    fn url(&self, path: &str) -> Result<Url, BackupClientError> {
        self.base_url
            .join(path)
            .map_err(|_| BackupClientError::Protocol("backup endpoint URL is invalid"))
    }
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
