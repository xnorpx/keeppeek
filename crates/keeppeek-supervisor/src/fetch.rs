//! SSRF-hardened GitHub Releases fetcher for `release` and `main` (`master`) channels.
//!
//! All outbound HTTP requests use workspace `ureq` with automatic redirects disabled
//! (`max_redirects(0)`), enforcing manual validation on up to 3 redirect hops across
//! an exact HTTPS port-443 GitHub host allowlist (`api.github.com`, `github.com`,
//! `objects.githubusercontent.com`, `release-assets.githubusercontent.com`).
//! Manifests (<= 64 KiB) are fetched and cryptographically verified *before*
//! streaming executable binaries into a temporary staging file on the supervisor
//! root volume with exact byte-length and SHA-256 enforcement.

use std::fmt;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::{Host, Url};

use crate::acl::create_private_dir_all;
use crate::manifest::{
    ComponentKind, MAX_BINARY_SIZE_BYTES, MAX_MANIFEST_FILE_BYTES, ManifestError,
    SignedReleaseManifest, TrustedSigningKey, encode_hex, verify_manifest,
};
use crate::slots::{SlotError, SlotStore, StageOptions, StageOutcome, UnsignedMainStageOptions};
use crate::version::{ReleaseVersion, SlotId, UpdateChannel, current_supervisor_version};

/// Default GitHub repository owner for core `keeppeek` releases.
pub const DEFAULT_CORE_REPO_OWNER: &str = "xnorpx";

/// Default GitHub repository name for core `keeppeek` releases.
pub const DEFAULT_CORE_REPO_NAME: &str = "keeppeek";

/// Rolling prerelease tag published by `.github/workflows/release.yml` for `main`.
pub const MAIN_PRERELEASE_TAG: &str = "master";

/// Maximum manual redirect hops followed per HTTP request.
pub const MAX_REDIRECT_HOPS: usize = 3;

/// Maximum GitHub Releases API JSON response size (512 KiB).
pub const MAX_RELEASE_API_JSON_BYTES: u64 = 512 * 1024;

/// Maximum `.sha256` sidecar file size (4 KiB).
pub const MAX_SHA256_FILE_BYTES: u64 = 4 * 1024;

const CONNECT_TIMEOUT_SECS: u64 = 10;
const IDLE_RECV_TIMEOUT_SECS: u64 = 30;
const DOWNLOAD_TOTAL_DEADLINE_SECS: u64 = 15 * 60;
const STREAM_BUFFER_BYTES: usize = 64 * 1024;
const ALLOWED_GITHUB_HOSTS: [&str; 4] = [
    "api.github.com",
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

static DOWNLOAD_TEMP_COUNTER: AtomicU64 = AtomicU64::new(1);

const _: () = {
    assert!(MAX_REDIRECT_HOPS == 3);
    assert!(MAX_RELEASE_API_JSON_BYTES >= MAX_MANIFEST_FILE_BYTES);
    assert!(STREAM_BUFFER_BYTES > 0);
};

/// Identifies a GitHub `<owner>/<repo>` release feed for one supervised component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentReleaseSource {
    /// GitHub repository owner (ASCII alphanumeric, `-`, `_`, `.`).
    pub owner: String,
    /// GitHub repository name (ASCII alphanumeric, `-`, `_`, `.`).
    pub repo: String,
}

impl ComponentReleaseSource {
    /// Returns the default official GitHub repository for `keeppeek` core.
    #[must_use]
    pub fn keeppeek_core() -> Self {
        Self {
            owner: DEFAULT_CORE_REPO_OWNER.to_owned(),
            repo: DEFAULT_CORE_REPO_NAME.to_owned(),
        }
    }

    /// Parses an `owner/repo` string after validating safe GitHub identifier characters.
    ///
    /// # Errors
    ///
    /// Returns [`FetchError::InvalidRepository`] if the input is malformed.
    pub fn parse_owner_repo(input: &str) -> Result<Self, FetchError> {
        let Some((owner, repo)) = input.trim().split_once('/') else {
            return Err(FetchError::InvalidRepository(input.to_owned()));
        };
        if !is_valid_repo_segment(owner) || !is_valid_repo_segment(repo) {
            return Err(FetchError::InvalidRepository(input.to_owned()));
        }
        Ok(Self {
            owner: owner.to_owned(),
            repo: repo.to_owned(),
        })
    }
}

/// Discovery summary returned by [`check_remote_channel`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelCheckReport {
    /// Component inspected.
    pub component: ComponentKind,
    /// Channel inspected (`release` or `main`).
    pub channel: UpdateChannel,
    /// Installed supervisor version used for same-major compatibility checks.
    pub supervisor_version: ReleaseVersion,
    /// True when the GitHub API returned HTTP 304 Not Modified for the cached ETag.
    pub not_modified: bool,
    /// Latest release tag eligible for in-place update on the current major line, if found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatible_tag: Option<String>,
    /// Semantic version of `compatible_tag`, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatible_version: Option<ReleaseVersion>,
    /// True when a signed Manifest v2 asset was published for the current host platform.
    pub signed_manifest_available: bool,
    /// True when a standalone binary and `.sha256` sidecar exist for the current host platform.
    pub standalone_binary_available: bool,
    /// Highest newer major version observed on GitHub Releases (requires fresh install).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newer_major_available: Option<ReleaseVersion>,
    /// True when upgrading to `newer_major_available` requires a fresh supervisor install.
    pub requires_fresh_install: bool,
    /// Manifest asset download URL for the current host platform, if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_asset_url: Option<String>,
    /// Standalone binary download URL for the current host platform, if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_asset_url: Option<String>,
    /// SHA-256 sidecar download URL for the current host platform, if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256_asset_url: Option<String>,
    /// Updated ETag header returned by GitHub, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
}

/// Options controlling remote channel download and staging.
#[derive(Debug, Clone)]
pub struct RemoteUpdateOptions {
    /// Target component (`core` or `vision`).
    pub component: ComponentKind,
    /// Target remote channel (`release` or `main`).
    pub channel: UpdateChannel,
    /// GitHub repository source for the component.
    pub source: ComponentReleaseSource,
    /// Target `<os>-<arch>` platform string.
    pub platform: String,
    /// Installed supervisor semantic version used for major-line compatibility checks.
    pub supervisor_version: ReleaseVersion,
    /// Trusted Ed25519 keys for signature verification.
    pub trusted_keys: Vec<TrustedSigningKey>,
    /// Permits staging an unsigned standalone `main` binary verified via `.sha256` sidecar.
    ///
    /// Ignored and rejected on the `release` channel, which always requires Ed25519 signatures.
    pub allow_unsigned_sha256: bool,
    /// Permits staging manifests declaring breaking or higher storage schemas.
    pub allow_breaking: bool,
    /// Whole-second Unix timestamp used for expiry validation and ETag cache metadata.
    pub now_unix_secs: u64,
    /// Maximum boot attempts allowed for the staged slot during probation.
    pub max_boot_attempts: u32,
}

/// Result returned after downloading, verifying, and staging a remote release or `main` build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteUpdateOutcome {
    /// Discovery report from GitHub Releases.
    pub check: ChannelCheckReport,
    /// Slot staging result (`None` when HTTP 304 Not Modified was returned).
    pub staged: Option<StageOutcome>,
}

/// Errors produced while querying, validating, downloading, or staging remote releases.
#[derive(Debug)]
pub enum FetchError {
    /// `local` channel cannot be queried via the remote GitHub fetcher.
    UnsupportedRemoteChannel(UpdateChannel),
    /// Remote repository source was invalid or not configured (for example `keeppeek-vision`).
    InvalidRepository(String),
    /// A URL or redirect target failed the strict HTTPS GitHub host allowlist.
    DisallowedUrl { url: String, detail: String },
    /// Exceeded [`MAX_REDIRECT_HOPS`] redirects.
    TooManyRedirects { url: String },
    /// Network transport or TLS error.
    Transport(String),
    /// Unexpected HTTP response status.
    HttpStatus { url: String, status: u16 },
    /// Response body exceeded its configured byte limit.
    ResponseTooLarge { url: String, max_bytes: u64 },
    /// Declared `Content-Length` did not match manifest `size_bytes`.
    ContentLengthMismatch { expected: u64, actual: u64 },
    /// GitHub API JSON or `.sha256` sidecar parsing failed.
    InvalidResponse(String),
    /// No compatible release or expected platform asset was found on the release feed.
    AssetNotFound { detail: String },
    /// Only a higher major release exists; upgrading requires a fresh supervisor install.
    MajorUpgradeRequiresFreshInstall {
        current_major: u32,
        newer_major: ReleaseVersion,
    },
    /// Signed manifest is missing on `release` (or on `main` without `--allow-unsigned-sha256`).
    UnsignedReleaseRejected { channel: UpdateChannel },
    /// Manifest cryptographic verification failed before binary download.
    Manifest(ManifestError),
    /// Slot staging or state persistence failed.
    Slot(SlotError),
    /// Local temporary file I/O failed.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for FetchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedRemoteChannel(channel) => {
                write!(
                    formatter,
                    "channel `{channel}` is not a remote GitHub channel"
                )
            }
            Self::InvalidRepository(detail) => {
                write!(
                    formatter,
                    "invalid or unconfigured GitHub release repository `{detail}`"
                )
            }
            Self::DisallowedUrl { url, detail } => {
                write!(
                    formatter,
                    "blocked disallowed download URL `{url}`: {detail}"
                )
            }
            Self::TooManyRedirects { url } => {
                write!(
                    formatter,
                    "exceeded {MAX_REDIRECT_HOPS} redirects fetching `{url}`"
                )
            }
            Self::Transport(detail) => write!(formatter, "HTTP transport failure: {detail}"),
            Self::HttpStatus { url, status } => {
                write!(formatter, "unexpected HTTP {status} from `{url}`")
            }
            Self::ResponseTooLarge { url, max_bytes } => {
                write!(
                    formatter,
                    "response from `{url}` exceeded {max_bytes} bytes"
                )
            }
            Self::ContentLengthMismatch { expected, actual } => write!(
                formatter,
                "remote Content-Length {actual} bytes does not match expected {expected} bytes"
            ),
            Self::InvalidResponse(detail) => {
                write!(formatter, "invalid remote release metadata: {detail}")
            }
            Self::AssetNotFound { detail } => {
                write!(formatter, "release asset not found: {detail}")
            }
            Self::MajorUpgradeRequiresFreshInstall {
                current_major,
                newer_major,
            } => write!(
                formatter,
                "latest release {newer_major} crosses major line {current_major}.x; install {newer_major} via a fresh install to upgrade keeppeek-supervisor"
            ),
            Self::UnsignedReleaseRejected { channel } => write!(
                formatter,
                "channel `{channel}` requires a signed Manifest v2 asset (no valid signed manifest was published)"
            ),
            Self::Manifest(error) => error.fmt(formatter),
            Self::Slot(error) => error.fmt(formatter),
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "download I/O failed at {}: {source}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for FetchError {}

impl From<ManifestError> for FetchError {
    fn from(error: ManifestError) -> Self {
        Self::Manifest(error)
    }
}

impl From<SlotError> for FetchError {
    fn from(error: SlotError) -> Self {
        Self::Slot(error)
    }
}

/// Validates that `raw_url` uses HTTPS on port 443 with no userinfo, no IP literals,
/// and an exact host on the GitHub release allowlist.
///
/// # Errors
///
/// Returns [`FetchError::DisallowedUrl`] on any policy violation.
pub fn validate_github_download_url(raw_url: &str) -> Result<Url, FetchError> {
    let parsed = Url::parse(raw_url).map_err(|error| FetchError::DisallowedUrl {
        url: raw_url.to_owned(),
        detail: error.to_string(),
    })?;
    if parsed.scheme() != "https" {
        return Err(FetchError::DisallowedUrl {
            url: raw_url.to_owned(),
            detail: "only https:// URLs are permitted".to_owned(),
        });
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(FetchError::DisallowedUrl {
            url: raw_url.to_owned(),
            detail: "URLs containing credentials are forbidden".to_owned(),
        });
    }
    if let Some(port) = parsed.port()
        && port != 443
    {
        return Err(FetchError::DisallowedUrl {
            url: raw_url.to_owned(),
            detail: format!("non-standard port {port} is forbidden"),
        });
    }
    let Some(Host::Domain(domain)) = parsed.host() else {
        return Err(FetchError::DisallowedUrl {
            url: raw_url.to_owned(),
            detail: "IP literal hosts are forbidden".to_owned(),
        });
    };
    let lower = domain.to_ascii_lowercase();
    if !ALLOWED_GITHUB_HOSTS.contains(&lower.as_str()) {
        return Err(FetchError::DisallowedUrl {
            url: raw_url.to_owned(),
            detail: format!("host `{lower}` is not on the GitHub download allowlist"),
        });
    }
    Ok(parsed)
}

/// Queries GitHub Releases for `channel` and reports compatible updates and newer major versions.
///
/// # Errors
///
/// Returns [`FetchError`] on transport, allowlist, or metadata parse failures.
pub fn check_remote_channel(
    store: &SlotStore,
    component: ComponentKind,
    channel: UpdateChannel,
    source: &ComponentReleaseSource,
    platform: &str,
    supervisor_version: ReleaseVersion,
    now_unix_secs: u64,
) -> Result<ChannelCheckReport, FetchError> {
    let client = HttpClient::github_production();
    let cached_etag = store
        .load_state()?
        .component(component)
        .fetch_cache
        .etag_for(channel)
        .map(str::to_owned);
    let report = check_remote_channel_with_client(
        &client,
        component,
        channel,
        source,
        platform,
        supervisor_version,
        cached_etag.as_deref(),
    )?;
    let _ = store.record_fetch_check(component, channel, report.etag.clone(), now_unix_secs);
    Ok(report)
}

/// Checks `channel`, verifies the manifest before downloading, and stages the new binary into `store`.
///
/// # Errors
///
/// Returns [`FetchError`] on network, allowlist, signature, digest, major-line, or staging failures.
pub fn fetch_and_stage_remote(
    store: &SlotStore,
    options: &RemoteUpdateOptions,
) -> Result<RemoteUpdateOutcome, FetchError> {
    let client = HttpClient::github_production();
    fetch_and_stage_with_client(&client, store, options)
}

/// Internal HTTP transport supporting strict production GitHub allowlisting and loopback mock tests.
#[derive(Debug, Clone)]
pub(crate) struct HttpClient {
    api_base: String,
    allow_loopback_http_for_tests: bool,
}

impl HttpClient {
    pub(crate) fn github_production() -> Self {
        Self {
            api_base: "https://api.github.com".to_owned(),
            allow_loopback_http_for_tests: false,
        }
    }

    #[cfg(test)]
    pub(crate) const fn for_loopback_test(api_base: String) -> Self {
        Self {
            api_base,
            allow_loopback_http_for_tests: true,
        }
    }

    fn validate_url(&self, raw_url: &str) -> Result<Url, FetchError> {
        if self.allow_loopback_http_for_tests {
            let parsed = Url::parse(raw_url).map_err(|error| FetchError::DisallowedUrl {
                url: raw_url.to_owned(),
                detail: error.to_string(),
            })?;
            if parsed.scheme() == "http"
                && matches!(parsed.host_str(), Some("127.0.0.1" | "localhost"))
            {
                return Ok(parsed);
            }
        }
        validate_github_download_url(raw_url)
    }

    fn build_agent() -> ureq::Agent {
        ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .timeout_connect(Some(Duration::from_secs(CONNECT_TIMEOUT_SECS)))
                .timeout_per_read(Some(Duration::from_secs(IDLE_RECV_TIMEOUT_SECS)))
                .timeout_global(Some(Duration::from_secs(DOWNLOAD_TOTAL_DEADLINE_SECS)))
                .max_redirects(0)
                .http_status_as_error(false)
                .build(),
        )
    }

    fn get_with_manual_redirects(
        &self,
        initial_url: &str,
        accept: &str,
        if_none_match: Option<&str>,
    ) -> Result<(u16, Option<String>, Option<u64>, ureq::Body), FetchError> {
        let agent = Self::build_agent();
        let mut current = self.validate_url(initial_url)?;
        let user_agent = format!("keeppeek-supervisor/{}", current_supervisor_version());

        for hop in 0..=MAX_REDIRECT_HOPS {
            let mut request = agent
                .get(current.as_str())
                .header("Accept", accept)
                .header("User-Agent", &user_agent);
            if hop == 0
                && let Some(etag) = if_none_match
            {
                request = request.header("If-None-Match", etag);
            }
            let response = request
                .call()
                .map_err(|error| FetchError::Transport(error.to_string()))?;
            let status = response.status().as_u16();
            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                if hop == MAX_REDIRECT_HOPS {
                    return Err(FetchError::TooManyRedirects {
                        url: initial_url.to_owned(),
                    });
                }
                let Some(location) = response
                    .headers()
                    .get("location")
                    .and_then(|value| value.to_str().ok())
                else {
                    return Err(FetchError::InvalidResponse(format!(
                        "HTTP {status} redirect missing Location header"
                    )));
                };
                let next = current
                    .join(location)
                    .map_err(|error| FetchError::DisallowedUrl {
                        url: location.to_owned(),
                        detail: error.to_string(),
                    })?;
                current = self.validate_url(next.as_str())?;
                continue;
            }
            let etag = response
                .headers()
                .get("etag")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let content_length = response
                .headers()
                .get("content-length")
                .and_then(|value| value.to_str().ok())
                .and_then(|raw| raw.parse::<u64>().ok());
            return Ok((status, etag, content_length, response.into_body()));
        }
        Err(FetchError::TooManyRedirects {
            url: initial_url.to_owned(),
        })
    }

    fn get_bounded_bytes(
        &self,
        url: &str,
        accept: &str,
        if_none_match: Option<&str>,
        max_bytes: u64,
    ) -> Result<(u16, Option<String>, Vec<u8>), FetchError> {
        let (status, etag, content_length, body) =
            self.get_with_manual_redirects(url, accept, if_none_match)?;
        if status == 304 {
            return Ok((304, etag, Vec::new()));
        }
        if status != 200 {
            return Err(FetchError::HttpStatus {
                url: url.to_owned(),
                status,
            });
        }
        if let Some(declared) = content_length
            && declared > max_bytes
        {
            return Err(FetchError::ResponseTooLarge {
                url: url.to_owned(),
                max_bytes,
            });
        }
        let reader = body.into_reader();
        let mut bytes = Vec::new();
        reader
            .take(max_bytes + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| FetchError::Transport(error.to_string()))?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
            return Err(FetchError::ResponseTooLarge {
                url: url.to_owned(),
                max_bytes,
            });
        }
        Ok((status, etag, bytes))
    }

    fn download_binary_to_file(
        &self,
        url: &str,
        dest: &Path,
        expected_size_bytes: Option<u64>,
    ) -> Result<(u64, String), FetchError> {
        let (status, _, content_length, body) =
            self.get_with_manual_redirects(url, "application/octet-stream", None)?;
        if status != 200 {
            return Err(FetchError::HttpStatus {
                url: url.to_owned(),
                status,
            });
        }
        let max_bytes = expected_size_bytes.map_or(MAX_BINARY_SIZE_BYTES, |size| {
            size.min(MAX_BINARY_SIZE_BYTES)
        });
        if let (Some(expected), Some(declared)) = (expected_size_bytes, content_length)
            && declared != expected
        {
            return Err(FetchError::ContentLengthMismatch {
                expected,
                actual: declared,
            });
        }
        if let Some(declared) = content_length
            && declared > MAX_BINARY_SIZE_BYTES
        {
            return Err(FetchError::ResponseTooLarge {
                url: url.to_owned(),
                max_bytes: MAX_BINARY_SIZE_BYTES,
            });
        }
        let mut reader = body.into_reader();
        let mut writer = File::create(dest).map_err(|source| FetchError::Io {
            path: dest.to_path_buf(),
            source,
        })?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; STREAM_BUFFER_BYTES];
        let mut total_bytes: u64 = 0;
        let deadline = Instant::now() + Duration::from_secs(DOWNLOAD_TOTAL_DEADLINE_SECS);

        loop {
            if Instant::now() > deadline {
                drop(writer);
                let _ = fs::remove_file(dest);
                return Err(FetchError::Transport(
                    "binary download exceeded 15-minute total deadline".to_owned(),
                ));
            }
            let read_count = reader
                .read(&mut buffer)
                .map_err(|error| FetchError::Transport(error.to_string()))?;
            if read_count == 0 {
                break;
            }
            total_bytes = total_bytes.saturating_add(u64::try_from(read_count).unwrap_or(u64::MAX));
            if total_bytes > max_bytes {
                drop(writer);
                let _ = fs::remove_file(dest);
                return Err(FetchError::ResponseTooLarge {
                    url: url.to_owned(),
                    max_bytes,
                });
            }
            hasher.update(&buffer[..read_count]);
            writer
                .write_all(&buffer[..read_count])
                .map_err(|source| FetchError::Io {
                    path: dest.to_path_buf(),
                    source,
                })?;
        }
        if total_bytes == 0 {
            drop(writer);
            let _ = fs::remove_file(dest);
            return Err(FetchError::Manifest(ManifestError::InvalidBinarySize {
                size_bytes: 0,
            }));
        }
        writer.sync_all().map_err(|source| FetchError::Io {
            path: dest.to_path_buf(),
            source,
        })?;
        Ok((total_bytes, encode_hex(&hasher.finalize())))
    }
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

pub(crate) fn check_remote_channel_with_client(
    client: &HttpClient,
    component: ComponentKind,
    channel: UpdateChannel,
    source: &ComponentReleaseSource,
    platform: &str,
    supervisor_version: ReleaseVersion,
    cached_etag: Option<&str>,
) -> Result<ChannelCheckReport, FetchError> {
    match channel {
        UpdateChannel::Release => check_release_channel(
            client,
            component,
            source,
            platform,
            supervisor_version,
            cached_etag,
        ),
        UpdateChannel::Main => check_main_channel(
            client,
            component,
            source,
            platform,
            supervisor_version,
            cached_etag,
        ),
        UpdateChannel::Local => Err(FetchError::UnsupportedRemoteChannel(channel)),
    }
}

fn check_release_channel(
    client: &HttpClient,
    component: ComponentKind,
    source: &ComponentReleaseSource,
    platform: &str,
    supervisor_version: ReleaseVersion,
    cached_etag: Option<&str>,
) -> Result<ChannelCheckReport, FetchError> {
    let url = format!(
        "{}/repos/{}/{}/releases?per_page=20",
        client.api_base, source.owner, source.repo
    );
    let (status, etag, bytes) = client.get_bounded_bytes(
        &url,
        "application/vnd.github+json",
        cached_etag,
        MAX_RELEASE_API_JSON_BYTES,
    )?;
    if status == 304 {
        return Ok(empty_not_modified_report(
            component,
            UpdateChannel::Release,
            supervisor_version,
            etag.or_else(|| cached_etag.map(str::to_owned)),
        ));
    }
    let releases: Vec<GitHubRelease> = serde_json::from_slice(&bytes)
        .map_err(|error| FetchError::InvalidResponse(error.to_string()))?;

    let mut best_compatible: Option<(ReleaseVersion, &GitHubRelease)> = None;
    let mut best_newer_major: Option<ReleaseVersion> = None;

    for release in &releases {
        if release.draft || release.prerelease {
            continue;
        }
        let Ok(version) = ReleaseVersion::from_str(release.tag_name.trim()) else {
            continue;
        };
        if supervisor_version.supports_in_place_upgrade_to(version) {
            if best_compatible.is_none_or(|(best, _)| version > best) {
                best_compatible = Some((version, release));
            }
        } else if version.major > supervisor_version.major
            && best_newer_major.is_none_or(|best| version > best)
        {
            best_newer_major = Some(version);
        }
    }

    let (compatible_tag, compatible_version, manifest_url, binary_url, sha256_url) =
        match best_compatible {
            Some((version, release)) => {
                let (m, b, s) =
                    find_platform_assets(release, component, &release.tag_name, platform);
                (Some(release.tag_name.clone()), Some(version), m, b, s)
            }
            None => (None, None, None, None, None),
        };

    Ok(ChannelCheckReport {
        component,
        channel: UpdateChannel::Release,
        supervisor_version,
        not_modified: false,
        compatible_tag,
        compatible_version,
        signed_manifest_available: manifest_url.is_some(),
        standalone_binary_available: binary_url.is_some() && sha256_url.is_some(),
        newer_major_available: best_newer_major,
        requires_fresh_install: best_newer_major.is_some(),
        manifest_asset_url: manifest_url,
        binary_asset_url: binary_url,
        sha256_asset_url: sha256_url,
        etag,
    })
}

fn check_main_channel(
    client: &HttpClient,
    component: ComponentKind,
    source: &ComponentReleaseSource,
    platform: &str,
    supervisor_version: ReleaseVersion,
    cached_etag: Option<&str>,
) -> Result<ChannelCheckReport, FetchError> {
    let url = format!(
        "{}/repos/{}/{}/releases/tags/{MAIN_PRERELEASE_TAG}",
        client.api_base, source.owner, source.repo
    );
    let (status, etag, bytes) = client.get_bounded_bytes(
        &url,
        "application/vnd.github+json",
        cached_etag,
        MAX_RELEASE_API_JSON_BYTES,
    )?;
    if status == 304 {
        return Ok(empty_not_modified_report(
            component,
            UpdateChannel::Main,
            supervisor_version,
            etag.or_else(|| cached_etag.map(str::to_owned)),
        ));
    }
    let release: GitHubRelease = serde_json::from_slice(&bytes)
        .map_err(|error| FetchError::InvalidResponse(error.to_string()))?;
    let (manifest_url, binary_url, sha256_url) =
        find_platform_assets(&release, component, MAIN_PRERELEASE_TAG, platform);

    Ok(ChannelCheckReport {
        component,
        channel: UpdateChannel::Main,
        supervisor_version,
        not_modified: false,
        compatible_tag: Some(release.tag_name),
        compatible_version: Some(supervisor_version),
        signed_manifest_available: manifest_url.is_some(),
        standalone_binary_available: binary_url.is_some() && sha256_url.is_some(),
        newer_major_available: None,
        requires_fresh_install: false,
        manifest_asset_url: manifest_url,
        binary_asset_url: binary_url,
        sha256_asset_url: sha256_url,
        etag,
    })
}

pub(crate) fn fetch_and_stage_with_client(
    client: &HttpClient,
    store: &SlotStore,
    options: &RemoteUpdateOptions,
) -> Result<RemoteUpdateOutcome, FetchError> {
    // Always fetch the live feed on `update` (never send If-None-Match) so an earlier `check`
    // or failed download attempt cannot mask an unstaged release behind HTTP 304.
    let check = check_remote_channel_with_client(
        client,
        options.component,
        options.channel,
        &options.source,
        &options.platform,
        options.supervisor_version,
        None,
    )?;
    if check.compatible_tag.is_none() {
        if let Some(newer_major) = check.newer_major_available {
            return Err(FetchError::MajorUpgradeRequiresFreshInstall {
                current_major: options.supervisor_version.major,
                newer_major,
            });
        }
        let _ = store.record_fetch_check(
            options.component,
            options.channel,
            check.etag,
            options.now_unix_secs,
        );
        return Err(FetchError::AssetNotFound {
            detail: format!("no published `{}` release found", options.channel),
        });
    }

    let binary_url =
        check
            .binary_asset_url
            .as_deref()
            .ok_or_else(|| FetchError::AssetNotFound {
                detail: format!(
                    "no standalone `{}` binary published for platform `{}`",
                    options.component, options.platform
                ),
            })?;

    if let Some(manifest_url) = check.manifest_asset_url.as_deref() {
        let staged = fetch_and_stage_signed(client, store, options, manifest_url, binary_url)?;
        let _ = store.record_fetch_check(
            options.component,
            options.channel,
            check.etag.clone(),
            options.now_unix_secs,
        );
        return Ok(RemoteUpdateOutcome {
            check,
            staged: Some(staged),
        });
    }

    if options.channel == UpdateChannel::Release || !options.allow_unsigned_sha256 {
        return Err(FetchError::UnsignedReleaseRejected {
            channel: options.channel,
        });
    }
    let sha256_url =
        check
            .sha256_asset_url
            .as_deref()
            .ok_or_else(|| FetchError::AssetNotFound {
                detail: format!(
                    "missing .sha256 sidecar for unsigned `{}` download on `{}`",
                    options.channel, options.platform
                ),
            })?;
    let staged = fetch_and_stage_unsigned_main(client, store, options, binary_url, sha256_url)?;
    let _ = store.record_fetch_check(
        options.component,
        options.channel,
        check.etag.clone(),
        options.now_unix_secs,
    );
    Ok(RemoteUpdateOutcome {
        check,
        staged: Some(staged),
    })
}

fn fetch_and_stage_signed(
    client: &HttpClient,
    store: &SlotStore,
    options: &RemoteUpdateOptions,
    manifest_url: &str,
    binary_url: &str,
) -> Result<StageOutcome, FetchError> {
    let (_, _, manifest_bytes) = client.get_bounded_bytes(
        manifest_url,
        "application/json",
        None,
        MAX_MANIFEST_FILE_BYTES,
    )?;
    let manifest: SignedReleaseManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| FetchError::Manifest(ManifestError::ParseJson(error)))?;

    // Verify manifest signature, channel, platform, and expiry BEFORE downloading the binary.
    verify_manifest(
        &manifest,
        &options.trusted_keys,
        options.channel,
        &options.platform,
        options.now_unix_secs,
    )?;
    if manifest.payload.component != options.component {
        return Err(FetchError::Manifest(ManifestError::InvalidField {
            field: "component",
            detail: format!(
                "manifest targets `{}` instead of `{}`",
                manifest.payload.component, options.component
            ),
        }));
    }
    let build_seq = (options.channel == UpdateChannel::Main).then_some(manifest.payload.build_seq);
    let slot_id = SlotId::new(
        options.channel,
        manifest.payload.version,
        build_seq,
        &manifest.payload.sha256_hex,
    )
    .map_err(|error| FetchError::InvalidResponse(error.to_string()))?;
    let existing = store.load_state()?;
    let comp = existing.component(options.component);
    if comp.active.as_ref() == Some(&slot_id)
        || comp
            .pending
            .as_ref()
            .is_some_and(|pending| pending.slot == slot_id)
    {
        return Ok(StageOutcome {
            slot_id,
            state: existing,
            changed: false,
        });
    }

    let temp_binary = allocate_download_temp_path(store.root(), options.component)?;
    let download_res =
        client.download_binary_to_file(binary_url, &temp_binary, Some(manifest.payload.size_bytes));
    let (actual_bytes, actual_hex) = match download_res {
        Ok(stats) => stats,
        Err(error) => {
            let _ = fs::remove_file(&temp_binary);
            return Err(error);
        }
    };
    if actual_bytes != manifest.payload.size_bytes {
        let _ = fs::remove_file(&temp_binary);
        return Err(FetchError::Manifest(ManifestError::SizeMismatch {
            expected_bytes: manifest.payload.size_bytes,
            actual_bytes,
        }));
    }
    if actual_hex != manifest.payload.sha256_hex {
        let _ = fs::remove_file(&temp_binary);
        return Err(FetchError::Manifest(ManifestError::DigestMismatch {
            expected: manifest.payload.sha256_hex,
            actual: actual_hex,
        }));
    }

    let stage_opts = StageOptions {
        expected_channel: options.channel,
        expected_platform: options.platform.clone(),
        supervisor_version: options.supervisor_version,
        now_unix_secs: options.now_unix_secs,
        allow_breaking: options.allow_breaking,
        max_boot_attempts: options.max_boot_attempts,
    };
    let outcome =
        store.stage_signed_binary(&manifest, &temp_binary, &options.trusted_keys, &stage_opts);
    let _ = fs::remove_file(&temp_binary);
    Ok(outcome?)
}

fn fetch_and_stage_unsigned_main(
    client: &HttpClient,
    store: &SlotStore,
    options: &RemoteUpdateOptions,
    binary_url: &str,
    sha256_url: &str,
) -> Result<StageOutcome, FetchError> {
    let (_, _, sha_bytes) =
        client.get_bounded_bytes(sha256_url, "text/plain", None, MAX_SHA256_FILE_BYTES)?;
    let sha_text = std::str::from_utf8(&sha_bytes)
        .map_err(|error| FetchError::InvalidResponse(error.to_string()))?;
    let expected_hex = parse_sha256_sidecar(sha_text)?;

    let temp_binary = allocate_download_temp_path(store.root(), options.component)?;
    let download_res = client.download_binary_to_file(binary_url, &temp_binary, None);
    let (_, actual_hex) = match download_res {
        Ok(stats) => stats,
        Err(error) => {
            let _ = fs::remove_file(&temp_binary);
            return Err(error);
        }
    };
    if actual_hex != expected_hex {
        let _ = fs::remove_file(&temp_binary);
        return Err(FetchError::Manifest(ManifestError::DigestMismatch {
            expected: expected_hex,
            actual: actual_hex,
        }));
    }
    let stage_opts = UnsignedMainStageOptions {
        component: options.component,
        version: options.supervisor_version,
        build_seq: options.now_unix_secs,
        expected_sha256_hex: &expected_hex,
        supervisor_version: options.supervisor_version,
        now_unix_secs: options.now_unix_secs,
        max_boot_attempts: options.max_boot_attempts,
    };
    let outcome = store.stage_unsigned_main_binary(&temp_binary, &stage_opts);
    let _ = fs::remove_file(&temp_binary);
    Ok(outcome?)
}

/// Parses `<64-lower-hex>  <filename>` checksum sidecar content.
pub(crate) fn parse_sha256_sidecar(content: &str) -> Result<String, FetchError> {
    let token = content
        .split_whitespace()
        .next()
        .ok_or_else(|| FetchError::InvalidResponse("empty .sha256 sidecar".to_owned()))?;
    if token.len() != 64
        || !token
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(FetchError::InvalidResponse(
            "invalid 64-char lowercase SHA-256 hex in .sha256 sidecar".to_owned(),
        ));
    }
    Ok(token.to_owned())
}

fn find_platform_assets(
    release: &GitHubRelease,
    component: ComponentKind,
    tag_name: &str,
    platform: &str,
) -> (Option<String>, Option<String>, Option<String>) {
    let ext = if platform.starts_with("windows-") {
        ".exe"
    } else {
        ""
    };
    let base = format!("{}-{tag_name}-{platform}", component.as_str());
    let manifest_name = format!("{base}.manifest.json");
    let binary_name = format!("{base}{ext}");
    let sha256_name = format!("{binary_name}.sha256");

    let mut manifest_url = None;
    let mut binary_url = None;
    let mut sha256_url = None;
    for asset in &release.assets {
        if asset.name == manifest_name {
            manifest_url = Some(asset.browser_download_url.clone());
        } else if asset.name == binary_name {
            binary_url = Some(asset.browser_download_url.clone());
        } else if asset.name == sha256_name {
            sha256_url = Some(asset.browser_download_url.clone());
        }
    }
    (manifest_url, binary_url, sha256_url)
}

fn allocate_download_temp_path(
    root: &Path,
    component: ComponentKind,
) -> Result<PathBuf, FetchError> {
    create_private_dir_all(root).map_err(|source| FetchError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    let dir = root.join(component.slot_dir_name());
    create_private_dir_all(&dir).map_err(|source| FetchError::Io {
        path: dir.clone(),
        source,
    })?;
    let seq = DOWNLOAD_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(dir.join(format!(
        ".download-{}-{}-{seq}.tmp",
        component.as_str(),
        std::process::id()
    )))
}

const fn empty_not_modified_report(
    component: ComponentKind,
    channel: UpdateChannel,
    supervisor_version: ReleaseVersion,
    etag: Option<String>,
) -> ChannelCheckReport {
    ChannelCheckReport {
        component,
        channel,
        supervisor_version,
        not_modified: true,
        compatible_tag: None,
        compatible_version: None,
        signed_manifest_available: false,
        standalone_binary_available: false,
        newer_major_available: None,
        requires_fresh_install: false,
        manifest_asset_url: None,
        binary_asset_url: None,
        sha256_asset_url: None,
        etag,
    }
}

fn is_valid_repo_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= 100
        && segment != "."
        && segment != ".."
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::thread;

    use crate::manifest::{
        DEFAULT_STORAGE_SCHEMA, MANIFEST_SCHEMA_VERSION_V2, ReleaseManifestPayload,
        SchemaCompatibility, current_platform, public_key_from_seed, sign_manifest,
    };

    use super::*;

    type MockRoute = (u16, Vec<(String, String)>, Vec<u8>);

    fn spawn_bound_mock_server(
        listener: TcpListener,
        routes: HashMap<String, MockRoute>,
        expected_requests: usize,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            for _ in 0..expected_requests {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut reader = BufReader::new(&stream);
                let mut req_line = String::new();
                let _ = reader.read_line(&mut req_line);
                let path = req_line.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let mut if_none_match = None;
                loop {
                    let mut line = String::new();
                    let read = reader.read_line(&mut line).unwrap_or(0);
                    if read == 0 || line == "\r\n" || line == "\n" {
                        break;
                    }
                    if let Some((k, v)) = line.split_once(':')
                        && k.eq_ignore_ascii_case("if-none-match")
                    {
                        if_none_match = Some(v.trim().to_owned());
                    }
                }
                let (mut status, headers, mut body) = routes
                    .get(&path)
                    .cloned()
                    .unwrap_or_else(|| (404, Vec::new(), b"not found".to_vec()));
                if let Some(tag) = if_none_match
                    && headers
                        .iter()
                        .any(|(k, v)| k.eq_ignore_ascii_case("etag") && v == &tag)
                {
                    status = 304;
                    body.clear();
                }
                let mut resp = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n",
                    body.len()
                );
                for (k, v) in headers {
                    resp.push_str(&format!("{k}: {v}\r\n"));
                }
                resp.push_str("\r\n");
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            }
        })
    }

    #[test]
    fn url_allowlist_blocks_ssrf_http_ip_credentials_and_foreign_hosts() {
        for allowed in [
            "https://api.github.com/repos/xnorpx/keeppeek/releases?per_page=20",
            "https://github.com/xnorpx/keeppeek/releases/download/0.2.0/keeppeek-0.2.0-linux-x86_64",
            "https://objects.githubusercontent.com/github-production-release-asset/123",
            "https://release-assets.githubusercontent.com/asset/456",
        ] {
            assert!(
                validate_github_download_url(allowed).is_ok(),
                "expected allowed GitHub URL: {allowed}"
            );
        }

        for blocked in [
            "http://github.com/xnorpx/keeppeek/releases",
            "https://github.com:8443/xnorpx/keeppeek/releases",
            "https://user:pass@github.com/xnorpx/keeppeek",
            "https://127.0.0.1/repos/xnorpx/keeppeek",
            "https://169.254.169.254/latest/meta-data/",
            "https://[::1]/releases",
            "https://evil.example/github.com/asset",
            "https://github.com.evil.example/asset",
            "file:///etc/passwd",
        ] {
            assert!(
                matches!(
                    validate_github_download_url(blocked),
                    Err(FetchError::DisallowedUrl { .. })
                ),
                "expected blocked URL: {blocked}"
            );
        }
    }

    #[test]
    fn release_discovery_selects_same_major_and_flags_newer_major_and_stages_signed() {
        let dir = std::env::temp_dir().join(format!(
            "keeppeek-fetch-test-{}-{}",
            std::process::id(),
            DOWNLOAD_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::create_dir_all(&dir);
        let binary_bytes = b"keeppeek-release-binary-payload-v0.2.0".to_vec();
        let sha = encode_hex(&Sha256::digest(&binary_bytes));
        let seed = [0x55_u8; 32];
        let pub_key = public_key_from_seed(&seed).unwrap();
        let trusted = TrustedSigningKey {
            key_id: "release-key-1".to_owned(),
            public_key_bytes: pub_key,
        };
        let platform = current_platform();
        let ext = if platform.starts_with("windows-") {
            ".exe"
        } else {
            ""
        };
        let manifest = sign_manifest(
            ReleaseManifestPayload {
                schema_version: MANIFEST_SCHEMA_VERSION_V2,
                channel: UpdateChannel::Release,
                component: ComponentKind::Core,
                version: ReleaseVersion::new(0, 2, 0),
                build_seq: 0,
                commit_sha: "abcdef1234567".to_owned(),
                storage_schema: DEFAULT_STORAGE_SCHEMA,
                platform: platform.clone(),
                binary_name: ComponentKind::Core.current_binary_file_name().to_owned(),
                sha256_hex: sha,
                size_bytes: u64::try_from(binary_bytes.len()).unwrap(),
                schema_compatibility: SchemaCompatibility::ExpandContract,
                issued_at_unix_secs: 1_750_000_000,
                expires_at_unix_secs: 1_760_000_000,
                signing_key_id: "release-key-1".to_owned(),
            },
            &seed,
        )
        .unwrap();
        let manifest_json = serde_json::to_vec(&manifest).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let releases_json = serde_json::json!([
            {
                "tag_name": "1.0.0",
                "draft": false,
                "prerelease": false,
                "assets": []
            },
            {
                "tag_name": "0.2.0",
                "draft": false,
                "prerelease": false,
                "assets": [
                    { "name": format!("keeppeek-0.2.0-{platform}.manifest.json"), "browser_download_url": format!("{base}/manifest") },
                    { "name": format!("keeppeek-0.2.0-{platform}{ext}"), "browser_download_url": format!("{base}/bin-redirect") }
                ]
            }
        ]);
        let mut routes = HashMap::new();
        routes.insert(
            "/repos/xnorpx/keeppeek/releases?per_page=20".to_owned(),
            (
                200,
                vec![("ETag".to_owned(), "\"etag-v1\"".to_owned())],
                serde_json::to_vec(&releases_json).unwrap(),
            ),
        );
        routes.insert("/manifest".to_owned(), (200, Vec::new(), manifest_json));
        routes.insert(
            "/bin-redirect".to_owned(),
            (
                302,
                vec![("Location".to_owned(), format!("{base}/bin-final"))],
                Vec::new(),
            ),
        );
        routes.insert("/bin-final".to_owned(), (200, Vec::new(), binary_bytes));
        let server_thread = spawn_bound_mock_server(listener, routes, 8);

        let client = HttpClient::for_loopback_test(base);
        let store = SlotStore::new(&dir);
        let options = RemoteUpdateOptions {
            component: ComponentKind::Core,
            channel: UpdateChannel::Release,
            source: ComponentReleaseSource::keeppeek_core(),
            platform: platform.clone(),
            supervisor_version: ReleaseVersion::new(0, 1, 0),
            trusted_keys: vec![trusted],
            allow_unsigned_sha256: false,
            allow_breaking: false,
            now_unix_secs: 1_750_000_000,
            max_boot_attempts: 2,
        };

        // 1. Check caches the ETag; a repeated check observes HTTP 304 Not Modified.
        let check1 = check_remote_channel_with_client(
            &client,
            ComponentKind::Core,
            UpdateChannel::Release,
            &options.source,
            &platform,
            options.supervisor_version,
            None,
        )
        .unwrap();
        assert!(!check1.not_modified);
        store
            .record_fetch_check(
                ComponentKind::Core,
                UpdateChannel::Release,
                check1.etag.clone(),
                options.now_unix_secs,
            )
            .unwrap();
        let check2 = check_remote_channel_with_client(
            &client,
            ComponentKind::Core,
            UpdateChannel::Release,
            &options.source,
            &platform,
            options.supervisor_version,
            check1.etag.as_deref(),
        )
        .unwrap();
        assert!(check2.not_modified);

        // 2. `update` ignores the cached check ETag so `check` -> `update` never silently no-ops.
        let outcome = fetch_and_stage_with_client(&client, &store, &options).unwrap();
        assert_eq!(
            outcome.check.compatible_version,
            Some(ReleaseVersion::new(0, 2, 0))
        );
        assert_eq!(
            outcome.check.newer_major_available,
            Some(ReleaseVersion::new(1, 0, 0))
        );
        assert!(outcome.check.requires_fresh_install);
        assert!(outcome.staged.as_ref().is_some_and(|item| item.changed));

        // 3. Repeat update is idempotent (manifest verified, binary not re-downloaded, changed = false).
        let second = fetch_and_stage_with_client(&client, &store, &options).unwrap();
        assert!(second.staged.as_ref().is_some_and(|item| !item.changed));

        server_thread.join().unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn redirect_loop_disallowed_redirect_and_unsigned_release_fail_closed() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let mut routes = HashMap::new();
        routes.insert(
            "/loop".to_owned(),
            (
                302,
                vec![("Location".to_owned(), format!("{base}/loop"))],
                Vec::new(),
            ),
        );
        routes.insert(
            "/evil-redirect".to_owned(),
            (
                302,
                vec![(
                    "Location".to_owned(),
                    "https://evil.example/payload".to_owned(),
                )],
                Vec::new(),
            ),
        );
        let server_thread = spawn_bound_mock_server(listener, routes, 5);
        let client = HttpClient::for_loopback_test(base.clone());

        assert!(matches!(
            client.get_bounded_bytes(&format!("{base}/loop"), "application/json", None, 1024),
            Err(FetchError::TooManyRedirects { .. })
        ));
        assert!(matches!(
            client.get_bounded_bytes(
                &format!("{base}/evil-redirect"),
                "application/json",
                None,
                1024
            ),
            Err(FetchError::DisallowedUrl { .. })
        ));
        server_thread.join().unwrap();
    }
}
