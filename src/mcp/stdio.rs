//! Stdio bridge CLI for connecting MCP clients to a KeepPeek server.
//!
//! `keeppeek mcp` reads newline-delimited JSON-RPC 2.0 requests from `stdin`,
//! forwards them over HTTP(S) to `POST /mcp`, and writes single-line JSON-RPC
//! responses strictly to `stdout`. All diagnostics go to `stderr`.

use crate::{
    camera_debug::store::{validate_artifact_name, validate_capture_id},
    config,
    logging::{LogEntry, LogLevel, LogSnapshot, TRACE_FILTER_MAX_BYTES},
    mcp::{
        protocol::{
            ERROR_INTERNAL, ERROR_INVALID_REQUEST, ERROR_PARSE, JsonRpcError, JsonRpcResponse,
            MAX_REQUEST_BODY_BYTES, READ_ONLY_HEADER,
        },
        skill,
    },
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fmt::Write as _,
    fs::File,
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tracing_subscriber::EnvFilter;
use ureq::Agent;
use url::{Host, Url};

const STDIO_HTTP_TIMEOUT: Duration = Duration::from_secs(60);
const SSE_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const SSE_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_STDIO_RESPONSE_BYTES: u64 = 2 * 1_024 * 1_024;
const MAX_LOG_SNAPSHOT_BYTES: u64 = 16 * 1_024 * 1_024;
const MAX_SSE_LINE_BYTES: usize = 256 * 1_024;
const MAX_SSE_DATA_BYTES: usize = 512 * 1_024;
const MAX_SSE_LOG_TAIL: usize = 1_000;
const READ_ARTIFACT_CHUNK_BYTES: usize = 65_536;
const MAX_PULL_ARTIFACT_BYTES: u64 = 256 * 1_024 * 1_024;
const MAX_PULL_CHUNKS: usize = 32_768;
const ACCESS_KEY_ENV: &str = "KEEPPEEK_ACCESS_KEY";

const _: () = {
    assert!(MAX_PULL_ARTIFACT_BYTES > 0);
    assert!(MAX_PULL_CHUNKS > 0);
};

/// Command-line options for `keeppeek mcp`.
#[derive(Debug, Clone, Args)]
#[command(name = "keeppeek mcp")]
pub struct McpArgs {
    /// Target KeepPeek server URL, such as `http://127.0.0.1:8081`.
    #[arg(long, value_name = "URL")]
    pub server: Option<String>,
    /// KeepPeek configuration file used to derive the local server URL.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// File containing a Bearer access key for remote MCP servers.
    #[arg(long, value_name = "PATH")]
    pub credential_file: Option<PathBuf>,
    /// Restrict this MCP session to read-only operations.
    #[arg(long)]
    pub read_only: bool,
    /// Optional MCP utility subcommand.
    #[command(subcommand)]
    pub command: Option<McpCommand>,
}

/// Log severity filter accepted by `keeppeek mcp logs --level`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LogLevelArg {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl From<LogLevelArg> for LogLevel {
    fn from(value: LogLevelArg) -> Self {
        match value {
            LogLevelArg::Trace => Self::Trace,
            LogLevelArg::Debug => Self::Debug,
            LogLevelArg::Info => Self::Info,
            LogLevelArg::Warn => Self::Warn,
            LogLevelArg::Error => Self::Error,
        }
    }
}

/// Subcommands available under `keeppeek mcp`.
#[derive(Debug, Clone, Subcommand)]
pub enum McpCommand {
    /// Print, list, or install bundled KeepPeek agent skills.
    Skill {
        /// Optional bundled skill name or alias (defaults to `keeppeek-mcp`).
        #[arg(value_name = "SKILL")]
        name: Option<String>,
        /// List available bundled skills with one-line descriptions.
        #[arg(long, conflicts_with_all = ["output", "name"])]
        list: bool,
        /// Directory where `SKILL.md` should be written instead of `stdout`.
        #[arg(long, value_name = "DIR")]
        output: Option<PathBuf>,
    },
    /// Download artifacts from a remote camera debug capture and verify SHA-256 digests.
    PullCapture {
        /// Capture identifier to download.
        capture_id: String,
        /// Local directory where downloaded artifact files will be written.
        #[arg(long, value_name = "DIR")]
        output: PathBuf,
        /// Specific artifact filenames to download (defaults to all capture artifacts).
        #[arg(long = "artifact", value_name = "NAME")]
        artifacts: Vec<String>,
    },
    /// Fetch snapshot logs or stream live SSE logs from a KeepPeek server.
    Logs {
        /// Minimum log level to display.
        #[arg(long, value_enum)]
        level: Option<LogLevelArg>,
        /// Filter entries whose target contains this substring (case-insensitive).
        #[arg(long)]
        target: Option<String>,
        /// Filter entries matching this camera ID or field substring.
        #[arg(long)]
        camera: Option<String>,
        /// Filter entries matching this substring across target, message, and fields.
        #[arg(long)]
        query: Option<String>,
        /// Temporarily set this EnvFilter directive via MCP and restore it on exit.
        #[arg(long)]
        filter: Option<String>,
        /// Stream live log events over `GET /logs` SSE instead of fetching a single snapshot.
        #[arg(long)]
        follow: bool,
        /// Maximum number of snapshot or initial tail entries.
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
}

/// HTTP client that forwards JSON-RPC requests to `POST /mcp`.
#[derive(Clone)]
pub struct McpHttpClient {
    agent: Agent,
    origin: Url,
    endpoint: Url,
    authorization: Option<String>,
    read_only: bool,
}

impl McpHttpClient {
    /// Builds a validated MCP HTTP client and enforces transport and credential rules.
    ///
    /// # Errors
    ///
    /// Returns an error when the server URL is invalid, when a non-loopback
    /// server lacks credentials, or when credentials would be sent over
    /// plaintext non-loopback HTTP.
    pub fn new(
        base_url: &str,
        access_key: Option<String>,
        read_only: bool,
    ) -> anyhow::Result<Self> {
        let mut parsed = Url::parse(base_url)
            .map_err(|error| anyhow::anyhow!("invalid MCP server URL: {error}"))?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.cannot_be_a_base()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            anyhow::bail!("MCP server URL must be an HTTP(S) origin without credentials");
        }
        let loopback = is_loopback(&parsed);
        let key = access_key
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        if key.is_none() && !loopback {
            anyhow::bail!(
                "remote MCP server requires a Bearer access key via {ACCESS_KEY_ENV} or --credential-file"
            );
        }
        if key.is_some() && parsed.scheme() == "http" && !loopback {
            anyhow::bail!("an access key requires HTTPS unless the MCP server is loopback");
        }
        let origin = parsed.clone();
        let base_path = parsed.path().trim_end_matches('/').to_owned();
        parsed.set_path(&format!("{base_path}/mcp"));
        let agent = Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(STDIO_HTTP_TIMEOUT))
            .build()
            .into();
        Ok(Self {
            agent,
            origin,
            endpoint: parsed,
            authorization: key.map(|token| format!("Bearer {token}")),
            read_only,
        })
    }

    /// Invokes one MCP tool via `POST /mcp` and returns its structured result object.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP request fails, returns JSON-RPC/tool errors, or omits JSON data.
    pub fn call_tool(&self, name: &str, arguments: Value) -> anyhow::Result<Value> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        });
        let request_json = serde_json::to_string(&request)?;
        let Some(body) = self
            .forward(&request_json)
            .map_err(|error| anyhow::anyhow!("{error}"))?
        else {
            anyhow::bail!("MCP server returned empty response for tool '{name}'");
        };
        let rpc: JsonRpcResponse = serde_json::from_str(&body)?;
        if let Some(error) = rpc.error {
            anyhow::bail!(
                "MCP tool '{name}' failed ({}): {}",
                error.code,
                error.message
            );
        }
        let result = rpc.result.ok_or_else(|| {
            anyhow::anyhow!("MCP tool '{name}' returned neither result nor error")
        })?;
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            let text = result
                .get("content")
                .and_then(|c| c.get(0))
                .and_then(|item| item.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("tool returned error");
            anyhow::bail!("MCP tool '{name}' error: {text}");
        }
        if let Some(structured) = result.get("structuredContent") {
            return Ok(structured.clone());
        }
        if let Some(text) = result
            .get("content")
            .and_then(|c| c.get(0))
            .and_then(|item| item.get("text"))
            .and_then(Value::as_str)
            && let Ok(parsed) = serde_json::from_str::<Value>(text)
        {
            return Ok(parsed);
        }
        Ok(result)
    }

    fn origin_url(&self, suffix: &str) -> Url {
        let mut url = self.origin.clone();
        let base_path = url.path().trim_end_matches('/').to_owned();
        url.set_path(&format!("{base_path}{suffix}"));
        url
    }

    /// Fetches `GET /logs/snapshot` using this client's origin and Bearer credentials.
    ///
    /// # Errors
    /// Returns an error if the HTTP request fails, returns non-2xx, or returns invalid JSON.
    pub fn get_logs_snapshot(&self) -> anyhow::Result<LogSnapshot> {
        let url = self.origin_url("/logs/snapshot");
        let mut request = self
            .agent
            .get(url.as_str())
            .header("Accept", "application/json");
        if let Some(authorization) = &self.authorization {
            request = request.header("Authorization", authorization);
        }
        let mut response = request
            .call()
            .map_err(|error| anyhow::anyhow!("GET /logs/snapshot failed: {error}"))?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_LOG_SNAPSHOT_BYTES)
            .read_to_string()
            .map_err(|error| anyhow::anyhow!("failed reading /logs/snapshot: {error}"))?;
        if !(200..300).contains(&status) {
            anyhow::bail!("GET /logs/snapshot returned HTTP {status}: {}", body.trim());
        }
        Ok(serde_json::from_str(&body)?)
    }

    /// Opens an SSE stream from `GET /logs?tail=<tail>` using this client's origin and credentials.
    ///
    /// # Errors
    /// Returns an error if connecting fails or the server responds with a non-2xx status.
    pub fn open_log_stream(&self, tail: usize) -> anyhow::Result<Box<dyn BufRead + Send>> {
        let mut url = self.origin_url("/logs");
        url.set_query(Some(&format!("tail={tail}")));
        let stream_agent: Agent = Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_connect(Some(SSE_CONNECT_TIMEOUT))
            .timeout_recv_response(Some(SSE_RESPONSE_TIMEOUT))
            .build()
            .into();
        let mut request = stream_agent
            .get(url.as_str())
            .header("Accept", "text/event-stream");
        if let Some(authorization) = &self.authorization {
            request = request.header("Authorization", authorization);
        }
        let mut response = request
            .call()
            .map_err(|error| anyhow::anyhow!("GET /logs failed: {error}"))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let body = response
                .body_mut()
                .with_config()
                .limit(8_192)
                .read_to_string()
                .unwrap_or_default();
            anyhow::bail!("GET /logs returned HTTP {status}: {}", body.trim());
        }
        Ok(Box::new(BufReader::new(response.into_body().into_reader())))
    }

    /// Sends one raw JSON-RPC payload to `POST /mcp` and returns any JSON response body.
    ///
    /// # Errors
    ///
    /// Returns an error string when the HTTP transport fails or the server returns
    /// an HTTP error status without a JSON-RPC body.
    pub fn forward(&self, request_json: &str) -> Result<Option<String>, String> {
        let mut request = self
            .agent
            .post(self.endpoint.as_str())
            .content_type("application/json")
            .header("Accept", "application/json");
        if let Some(authorization) = &self.authorization {
            request = request.header("Authorization", authorization);
        }
        if self.read_only {
            request = request.header(READ_ONLY_HEADER, "true");
        }
        let mut response = request
            .send(request_json)
            .map_err(|error| format!("MCP server request failed: {error}"))?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_STDIO_RESPONSE_BYTES)
            .read_to_string()
            .map_err(|error| format!("failed to read MCP server response: {error}"))?;
        if matches!(status, 202 | 204) || body.trim().is_empty() {
            if (200..300).contains(&status) {
                return Ok(None);
            }
            return Err(format!("MCP server returned HTTP {status}"));
        }
        if serde_json::from_str::<JsonRpcResponse>(&body).is_ok() {
            return Ok(Some(body));
        }
        if !(200..300).contains(&status) {
            return Err(format!(
                "MCP server returned HTTP {status}: {}",
                body.trim()
            ));
        }
        Ok(Some(body))
    }
}

/// Runs the `keeppeek mcp` CLI command.
///
/// # Errors
///
/// Returns an error when configuration, credential loading, skill writing, or
/// stdio framing fails.
pub fn run(args: McpArgs) -> anyhow::Result<()> {
    if let Some(McpCommand::Skill { name, list, output }) = args.command {
        let mut stdout = io::stdout().lock();
        return execute_skill_command(name.as_deref(), list, output.as_deref(), &mut stdout);
    }
    let server_url = resolve_server_url(args.server.as_deref(), args.config.as_deref())?;
    let credential = resolve_credential(args.credential_file.as_deref())?;
    let client = McpHttpClient::new(&server_url, credential, args.read_only)?;
    match args.command {
        Some(McpCommand::PullCapture {
            capture_id,
            output,
            artifacts,
        }) => {
            let mut stdout = io::stdout().lock();
            return execute_pull_capture(&client, &capture_id, &output, &artifacts, &mut stdout);
        }
        Some(McpCommand::Logs {
            level,
            target,
            camera,
            query,
            filter,
            follow,
            limit,
        }) => {
            let stop_flag = Arc::new(AtomicBool::new(false));
            let options = LogsCommandOptions {
                level: level.map(LogLevel::from),
                target,
                camera,
                query,
                filter,
                follow,
                limit,
            };
            let mut stdout = io::stdout().lock();
            return execute_logs_command(&client, &options, &stop_flag, true, &mut stdout);
        }
        Some(McpCommand::Skill { .. }) | None => {}
    }
    let stdin = io::stdin();
    let stdout = io::stdout();
    serve_stdio_loop(stdin.lock(), stdout.lock(), |line| {
        client.forward(line).inspect_err(|error| {
            eprintln!("keeppeek mcp: {error}");
        })
    })
}

/// Processes newline-delimited JSON-RPC 2.0 lines from `reader` and writes compact replies to `writer`.
///
/// Every non-empty line is checked against `MAX_REQUEST_BODY_BYTES`. Malformed
/// lines produce a JSON-RPC error line on `writer` without aborting the session.
///
/// # Errors
///
/// Returns an error when reading from `reader` or writing to `writer` fails.
pub fn serve_stdio_loop<R, W, F>(
    mut reader: R,
    mut writer: W,
    mut dispatch: F,
) -> anyhow::Result<()>
where
    R: BufRead,
    W: Write,
    F: FnMut(&str) -> Result<Option<String>, String>,
{
    let mut line_buffer = Vec::new();
    loop {
        line_buffer.clear();
        let outcome = read_bounded_line(&mut reader, &mut line_buffer, MAX_REQUEST_BODY_BYTES)?;
        let Some(within_limit) = outcome else {
            break;
        };
        if !within_limit {
            write_json_line(
                &mut writer,
                &JsonRpcResponse::error(
                    Value::Null,
                    JsonRpcError::new(
                        ERROR_INVALID_REQUEST,
                        format!("request line exceeds {MAX_REQUEST_BODY_BYTES} bytes"),
                    ),
                ),
            )?;
            continue;
        }
        let Ok(raw_line) = std::str::from_utf8(&line_buffer) else {
            write_json_line(
                &mut writer,
                &JsonRpcResponse::error(
                    Value::Null,
                    JsonRpcError::new(ERROR_PARSE, "request line must be valid UTF-8"),
                ),
            )?;
            continue;
        };
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        handle_stdio_line(trimmed, &mut writer, &mut dispatch)?;
    }
    Ok(())
}

fn handle_stdio_line<W, F>(trimmed: &str, writer: &mut W, dispatch: &mut F) -> anyhow::Result<()>
where
    W: Write,
    F: FnMut(&str) -> Result<Option<String>, String>,
{
    let parsed_value: Value = match serde_json::from_str(trimmed) {
        Ok(value) => value,
        Err(_) => {
            return write_json_line(
                writer,
                &JsonRpcResponse::error(
                    Value::Null,
                    JsonRpcError::new(ERROR_PARSE, "invalid JSON payload on stdin"),
                ),
            );
        }
    };
    let request_id = parsed_value
        .as_object()
        .and_then(|map| map.get("id"))
        .cloned();
    match dispatch(trimmed) {
        Ok(Some(response_json)) => {
            let compact = compact_json_line(&response_json, request_id.as_ref());
            writer.write_all(compact.as_bytes())?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
        Ok(None) => {}
        Err(error_message) => {
            if let Some(id) = request_id {
                write_json_line(
                    writer,
                    &JsonRpcResponse::error(id, JsonRpcError::new(ERROR_INTERNAL, error_message)),
                )?;
            }
        }
    }
    Ok(())
}

fn compact_json_line(raw: &str, request_id: Option<&Value>) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(raw)
        && let Ok(compact) = serde_json::to_string(&value)
    {
        return compact;
    }
    let fallback = JsonRpcResponse::error(
        request_id.cloned().unwrap_or(Value::Null),
        JsonRpcError::new(ERROR_INTERNAL, "MCP server returned invalid JSON"),
    );
    serde_json::to_string(&fallback).unwrap_or_else(|_| {
        r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"serialization failed"}}"#
            .to_owned()
    })
}

fn write_json_line<W: Write>(writer: &mut W, response: &JsonRpcResponse) -> anyhow::Result<()> {
    serde_json::to_writer(&mut *writer, response)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    max_bytes: usize,
) -> io::Result<Option<bool>> {
    let mut exceeded = false;
    let mut read_any = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(read_any.then_some(!exceeded));
        }
        read_any = true;
        let (consume, finished) = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or((available.len(), false), |index| (index + 1, true));
        if !exceeded {
            if buffer.len().saturating_add(consume) <= max_bytes {
                buffer.extend_from_slice(&available[..consume]);
            } else {
                exceeded = true;
                buffer.clear();
            }
        }
        reader.consume(consume);
        if finished {
            return Ok(Some(!exceeded));
        }
    }
}

fn execute_skill_command<W: Write>(
    skill_name: Option<&str>,
    list: bool,
    output_dir: Option<&Path>,
    writer: &mut W,
) -> anyhow::Result<()> {
    if list {
        anyhow::ensure!(
            output_dir.is_none(),
            "--list cannot be combined with --output"
        );
        anyhow::ensure!(
            skill_name.is_none(),
            "--list cannot be combined with a skill name"
        );
        for entry in skill::list_skills() {
            writeln!(writer, "{}\t{}", entry.name(), entry.description())?;
        }
        writer.flush()?;
        return Ok(());
    }
    let resolved = match skill_name {
        Some(name) => skill::resolve_skill(name)?,
        None => skill::BundledSkill::KeeppeekMcp,
    };
    if let Some(dir) = output_dir {
        let destination = skill::write_named_skill_to_dir(resolved, dir)?;
        writeln!(writer, "{}", destination.display())?;
        writer.flush()?;
        return Ok(());
    }
    writer.write_all(skill::render_named_skill_markdown(resolved).as_bytes())?;
    writer.flush()?;
    Ok(())
}

struct TempFileGuard(Option<PathBuf>);

impl TempFileGuard {
    const fn new(path: PathBuf) -> Self {
        Self(Some(path))
    }

    fn persist(mut self, target: &Path) -> io::Result<()> {
        let Some(temp_path) = self.0.as_ref() else {
            return Err(io::Error::other("temporary file already consumed"));
        };
        std::fs::rename(temp_path, target)?;
        self.0 = None;
        Ok(())
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn execute_pull_capture<W: Write>(
    client: &McpHttpClient,
    capture_id: &str,
    output_dir: &Path,
    requested_artifacts: &[String],
    writer: &mut W,
) -> anyhow::Result<()> {
    validate_capture_id(capture_id)?;
    let target_artifacts = resolve_pull_artifacts(client, capture_id, requested_artifacts)?;
    std::fs::create_dir_all(output_dir)?;
    for artifact in target_artifacts {
        let (bytes_written, sha256) =
            download_verified_artifact(client, capture_id, &artifact, output_dir)?;
        writeln!(writer, "{artifact}\t{bytes_written}\t{sha256}")?;
    }
    writer.flush()?;
    Ok(())
}

fn resolve_pull_artifacts(
    client: &McpHttpClient,
    capture_id: &str,
    requested: &[String],
) -> anyhow::Result<Vec<String>> {
    if !requested.is_empty() {
        let mut deduped = Vec::with_capacity(requested.len());
        for raw in requested {
            let name = raw.trim();
            validate_artifact_name(name)?;
            if !deduped.iter().any(|existing| existing == name) {
                deduped.push(name.to_owned());
            }
        }
        return Ok(deduped);
    }
    let listed = client.call_tool("camera_debugging", json!({ "action": "list_captures" }))?;
    let captures = listed
        .get("captures")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("list_captures response missing captures array"))?;
    let entry = captures
        .iter()
        .find(|item| item.get("capture_id").and_then(Value::as_str) == Some(capture_id))
        .ok_or_else(|| anyhow::anyhow!("capture '{capture_id}' was not found on server"))?;
    let files = entry
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("capture '{capture_id}' metadata missing files list"))?;
    let mut out = Vec::with_capacity(files.len());
    for file in files {
        let name = file.as_str().ok_or_else(|| {
            anyhow::anyhow!("capture '{capture_id}' contains non-string filename")
        })?;
        validate_artifact_name(name)?;
        if !out.iter().any(|existing| existing == name) {
            out.push(name.to_owned());
        }
    }
    anyhow::ensure!(
        !out.is_empty(),
        "capture '{capture_id}' has no downloadable artifacts"
    );
    Ok(out)
}

fn download_verified_artifact(
    client: &McpHttpClient,
    capture_id: &str,
    artifact: &str,
    output_dir: &Path,
) -> anyhow::Result<(u64, String)> {
    let final_path = output_dir.join(artifact);
    anyhow::ensure!(
        final_path.parent() == Some(output_dir),
        "artifact path escaped output directory"
    );
    let temp_path = output_dir.join(format!(".pull-{}.tmp", uuid::Uuid::new_v4()));
    let temp_guard = TempFileGuard::new(temp_path.clone());
    let mut out_file = File::create(&temp_path)?;
    let (written, sha256) = stream_artifact_chunks(client, capture_id, artifact, &mut out_file)?;
    out_file.sync_all()?;
    drop(out_file);
    temp_guard.persist(&final_path)?;
    Ok((written, sha256))
}

struct PullTransferState {
    hasher: Sha256,
    offset: u64,
    expected_sha256: Option<String>,
    expected_total: Option<u64>,
}

impl PullTransferState {
    fn new() -> Self {
        Self {
            hasher: Sha256::new(),
            offset: 0,
            expected_sha256: None,
            expected_total: None,
        }
    }

    fn verify_chunk_header(&mut self, artifact: &str, chunk: &Value) -> anyhow::Result<u64> {
        let server_sha = chunk_str(chunk, "sha256")?;
        if let Some(prev) = &self.expected_sha256 {
            anyhow::ensure!(
                prev == server_sha,
                "artifact '{artifact}' sha256 changed mid-download"
            );
        } else {
            self.expected_sha256 = Some(server_sha.to_owned());
        }
        let total_bytes = chunk_u64(chunk, "total_bytes")?;
        let pinned_total = match self.expected_total {
            Some(expected) => {
                anyhow::ensure!(
                    total_bytes == expected,
                    "artifact '{artifact}' total_bytes changed mid-download ({total_bytes} != {expected})"
                );
                expected
            }
            None => {
                anyhow::ensure!(
                    total_bytes <= MAX_PULL_ARTIFACT_BYTES,
                    "artifact '{artifact}' total_bytes ({total_bytes}) exceeds limit ({MAX_PULL_ARTIFACT_BYTES} bytes)"
                );
                self.expected_total = Some(total_bytes);
                total_bytes
            }
        };
        anyhow::ensure!(
            self.offset <= pinned_total,
            "artifact '{artifact}' offset ({}) exceeds expected total_bytes ({pinned_total})",
            self.offset
        );
        Ok(pinned_total)
    }

    fn apply_chunk(
        &mut self,
        artifact: &str,
        chunk: &Value,
        out_file: &mut File,
    ) -> anyhow::Result<bool> {
        let pinned_total = self.verify_chunk_header(artifact, chunk)?;
        let decoded = decode_artifact_payload(chunk)?;
        let expected_chunk_len = chunk_u64(chunk, "length")?;
        let actual_len = u64::try_from(decoded.len()).unwrap_or(u64::MAX);
        anyhow::ensure!(
            actual_len == expected_chunk_len,
            "artifact '{artifact}' chunk length mismatch"
        );
        let new_offset = self
            .offset
            .checked_add(actual_len)
            .ok_or_else(|| anyhow::anyhow!("artifact '{artifact}' offset overflow"))?;
        anyhow::ensure!(
            new_offset <= pinned_total,
            "artifact '{artifact}' chunk exceeds expected total_bytes ({new_offset} > {pinned_total})"
        );
        out_file.write_all(&decoded)?;
        self.hasher.update(&decoded);
        self.offset = new_offset;
        let next_offset = chunk_u64(chunk, "next_offset")?;
        let eof = chunk_bool(chunk, "eof")?;
        anyhow::ensure!(
            next_offset == self.offset,
            "artifact '{artifact}' next_offset mismatch ({next_offset} != {})",
            self.offset
        );
        if eof {
            return Ok(true);
        }
        anyhow::ensure!(
            expected_chunk_len > 0,
            "artifact '{artifact}' stalled without eof"
        );
        Ok(false)
    }
}

fn stream_artifact_chunks(
    client: &McpHttpClient,
    capture_id: &str,
    artifact: &str,
    out_file: &mut File,
) -> anyhow::Result<(u64, String)> {
    stream_artifact_chunks_bounded(client, capture_id, artifact, out_file, MAX_PULL_CHUNKS)
}

fn stream_artifact_chunks_bounded(
    client: &McpHttpClient,
    capture_id: &str,
    artifact: &str,
    out_file: &mut File,
    max_chunks: usize,
) -> anyhow::Result<(u64, String)> {
    let mut state = PullTransferState::new();
    let mut finished = false;
    for _ in 0..max_chunks {
        let chunk = client.call_tool(
            "camera_debugging",
            json!({
                "action": "read_artifact",
                "capture_id": capture_id,
                "artifact": artifact,
                "offset": state.offset,
                "length": READ_ARTIFACT_CHUNK_BYTES,
                "encoding": "auto"
            }),
        )?;
        if state.apply_chunk(artifact, &chunk, out_file)? {
            finished = true;
            break;
        }
    }
    anyhow::ensure!(
        finished,
        "artifact '{artifact}' exceeded maximum chunk count ({max_chunks})"
    );
    let expected_total = state.expected_total.unwrap_or(0);
    anyhow::ensure!(
        state.offset == expected_total,
        "artifact '{artifact}' truncated ({} != {expected_total} bytes)",
        state.offset
    );
    let actual_sha256 = digest_hex(&state.hasher.finalize());
    let server_sha256 = state.expected_sha256.unwrap_or_default();
    anyhow::ensure!(
        actual_sha256 == server_sha256,
        "artifact '{artifact}' SHA-256 mismatch: expected {server_sha256}, got {actual_sha256}"
    );
    Ok((state.offset, actual_sha256))
}

fn decode_artifact_payload(chunk: &Value) -> anyhow::Result<Vec<u8>> {
    let encoding = chunk_str(chunk, "encoding")?;
    let data = chunk_str(chunk, "data")?;
    match encoding {
        "utf8" => Ok(data.as_bytes().to_vec()),
        "base64" => Ok(BASE64.decode(data)?),
        other => anyhow::bail!("unsupported read_artifact encoding '{other}'"),
    }
}

fn chunk_str<'a>(value: &'a Value, field: &str) -> anyhow::Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("read_artifact response missing string '{field}'"))
}

fn chunk_u64(value: &Value, field: &str) -> anyhow::Result<u64> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("read_artifact response missing u64 '{field}'"))
}

fn chunk_bool(value: &Value, field: &str) -> anyhow::Result<bool> {
    value
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| anyhow::anyhow!("read_artifact response missing bool '{field}'"))
}

fn digest_hex(digest: &[u8]) -> String {
    let mut hex = String::with_capacity(digest.len().saturating_mul(2));
    for byte in digest {
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

struct LogsCommandOptions {
    level: Option<LogLevel>,
    target: Option<String>,
    camera: Option<String>,
    query: Option<String>,
    filter: Option<String>,
    follow: bool,
    limit: usize,
}

struct LogFilterMatcher {
    min_rank: Option<u8>,
    target_needle: Option<String>,
    camera_needle: Option<String>,
    query_needle: Option<String>,
}

impl LogFilterMatcher {
    fn from_options(options: &LogsCommandOptions) -> Self {
        Self {
            min_rank: options.level.map(LogLevel::rank),
            target_needle: options.target.as_deref().map(str::to_ascii_lowercase),
            camera_needle: options.camera.as_deref().map(str::to_ascii_lowercase),
            query_needle: options.query.as_deref().map(str::to_ascii_lowercase),
        }
    }

    fn matches(&self, entry: &LogEntry) -> bool {
        self.min_rank.is_none_or(|rank| entry.level.rank() >= rank)
            && self
                .target_needle
                .as_deref()
                .is_none_or(|needle| entry.target.to_ascii_lowercase().contains(needle))
            && self
                .camera_needle
                .as_deref()
                .is_none_or(|needle| entry.matches_query(needle))
            && self
                .query_needle
                .as_deref()
                .is_none_or(|needle| entry.matches_query(needle))
    }
}

struct RemoteFilterGuard {
    state: Arc<Mutex<Option<(McpHttpClient, String)>>>,
}

impl RemoteFilterGuard {
    fn acquire(client: &McpHttpClient, new_filter: &str) -> anyhow::Result<Self> {
        let trimmed = new_filter.trim();
        anyhow::ensure!(
            !trimmed.is_empty() && trimmed.len() <= TRACE_FILTER_MAX_BYTES,
            "filter must be between 1 and {TRACE_FILTER_MAX_BYTES} bytes"
        );
        EnvFilter::try_new(trimmed)?;
        let settings = client.call_tool("get_logging_settings", json!({}))?;
        // Prefer unboosted `base_filter` so ephemeral capture TraceFilterGuard boosts are not persisted.
        let previous = settings
            .get("base_filter")
            .or_else(|| settings.get("active_filter"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                anyhow::anyhow!("get_logging_settings missing base_filter/active_filter")
            })?
            .to_owned();
        client.call_tool("set_logging_filter", json!({ "filter": trimmed }))?;
        Ok(Self {
            state: Arc::new(Mutex::new(Some((client.clone(), previous)))),
        })
    }

    fn restore_now(state: &Arc<Mutex<Option<(McpHttpClient, String)>>>) {
        let mut guard = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((client, previous)) = guard.take()
            && let Err(error) =
                client.call_tool("set_logging_filter", json!({ "filter": previous }))
        {
            eprintln!("keeppeek mcp logs: failed to restore logging filter: {error}");
        }
    }
}

impl Drop for RemoteFilterGuard {
    fn drop(&mut self) {
        Self::restore_now(&self.state);
    }
}

fn execute_logs_command<W: Write>(
    client: &McpHttpClient,
    options: &LogsCommandOptions,
    stop_flag: &Arc<AtomicBool>,
    install_ctrlc: bool,
    writer: &mut W,
) -> anyhow::Result<()> {
    let _filter_guard = options
        .filter
        .as_deref()
        .map(|directive| RemoteFilterGuard::acquire(client, directive))
        .transpose()?;
    if install_ctrlc && (options.follow || _filter_guard.is_some()) {
        let ctrlc_stop = stop_flag.clone();
        let ctrlc_guard = _filter_guard.as_ref().map(|guard| guard.state.clone());
        let _ = ctrlc::set_handler(move || {
            ctrlc_stop.store(true, Ordering::Release);
            if let Some(state) = &ctrlc_guard {
                RemoteFilterGuard::restore_now(state);
            }
            std::process::exit(130);
        });
    }
    let matcher = LogFilterMatcher::from_options(options);
    if !options.follow {
        let snapshot = client.get_logs_snapshot()?;
        let limit = options.limit.max(1);
        let mut matched: Vec<LogEntry> = snapshot
            .entries
            .into_iter()
            .filter(|entry| matcher.matches(entry))
            .collect();
        if matched.len() > limit {
            matched.drain(..matched.len() - limit);
        }
        for entry in &matched {
            write_log_entry_line(writer, entry)?;
        }
        writer.flush()?;
        return Ok(());
    }
    let tail = options.limit.clamp(1, MAX_SSE_LOG_TAIL);
    let mut reader = client.open_log_stream(tail)?;
    stream_sse_logs(&mut reader, &matcher, writer, stop_flag)
}

fn stream_sse_logs<R: BufRead, W: Write>(
    reader: &mut R,
    matcher: &LogFilterMatcher,
    writer: &mut W,
    stop_flag: &AtomicBool,
) -> anyhow::Result<()> {
    let mut line_buf = Vec::new();
    let mut event_name = String::new();
    let mut data_buf = String::new();
    let mut discard_event = false;
    while !stop_flag.load(Ordering::Acquire) {
        line_buf.clear();
        let Some(within_limit) = read_bounded_line(reader, &mut line_buf, MAX_SSE_LINE_BYTES)?
        else {
            break;
        };
        if !within_limit {
            event_name.clear();
            data_buf.clear();
            discard_event = true;
            continue;
        }
        let line = String::from_utf8_lossy(&line_buf);
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            if !discard_event {
                dispatch_sse_event(&event_name, &data_buf, matcher, writer)?;
            }
            event_name.clear();
            data_buf.clear();
            discard_event = false;
            continue;
        }
        if discard_event || trimmed.starts_with(':') {
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("event:") {
            event_name = value.trim().to_owned();
        } else if let Some(value) = trimmed.strip_prefix("data:") {
            let chunk = value.trim_start();
            let separator_bytes = usize::from(!data_buf.is_empty());
            let next_len = data_buf
                .len()
                .saturating_add(separator_bytes)
                .saturating_add(chunk.len());
            if next_len <= MAX_SSE_DATA_BYTES {
                if !data_buf.is_empty() {
                    data_buf.push('\n');
                }
                data_buf.push_str(chunk);
            } else {
                event_name.clear();
                data_buf.clear();
                discard_event = true;
            }
        }
    }
    Ok(())
}

fn dispatch_sse_event<W: Write>(
    event_name: &str,
    data: &str,
    matcher: &LogFilterMatcher,
    writer: &mut W,
) -> anyhow::Result<()> {
    if data.is_empty() {
        return Ok(());
    }
    if event_name.is_empty() || event_name == "log" {
        if let Ok(entry) = serde_json::from_str::<LogEntry>(data)
            && matcher.matches(&entry)
        {
            write_log_entry_line(writer, &entry)?;
            writer.flush()?;
        }
        return Ok(());
    }
    if matches!(event_name, "gap" | "replay-truncated") {
        eprintln!("keeppeek mcp logs: event={event_name} data={data}");
    }
    Ok(())
}

fn write_log_entry_line<W: Write>(writer: &mut W, entry: &LogEntry) -> anyhow::Result<()> {
    serde_json::to_writer(&mut *writer, entry)?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn resolve_server_url(
    explicit_server: Option<&str>,
    config_path: Option<&Path>,
) -> anyhow::Result<String> {
    if let Some(server) = explicit_server {
        return Ok(server.to_owned());
    }
    if let Some(path) = config_path {
        let loaded = config::load_config(path)?;
        return Ok(server_url_from_config(&loaded));
    }
    let default_path = config::config_path();
    if default_path.exists()
        && let Ok(loaded) = config::load_config(&default_path)
    {
        return Ok(server_url_from_config(&loaded));
    }
    Ok("http://127.0.0.1:8081".to_owned())
}

fn server_url_from_config(cfg: &config::Config) -> String {
    let host = match cfg.host.as_str() {
        "" | "0.0.0.0" => "127.0.0.1",
        "::" | "[::]" => "[::1]",
        other if other.contains(':') && !other.starts_with('[') => {
            return format!("http://[{other}]:{}", cfg.port);
        }
        other => other,
    };
    format!("http://{host}:{}", cfg.port)
}

fn resolve_credential(credential_file: Option<&Path>) -> anyhow::Result<Option<String>> {
    if let Some(path) = credential_file {
        let raw = std::fs::read_to_string(path).map_err(|error| {
            anyhow::anyhow!(
                "unable to read MCP credential file {}: {error}",
                path.display()
            )
        })?;
        let trimmed = raw.trim();
        anyhow::ensure!(
            !trimmed.is_empty(),
            "MCP credential file {} is empty",
            path.display()
        );
        return Ok(Some(trimmed.to_owned()));
    }
    match std::env::var(ACCESS_KEY_ENV) {
        Ok(value) if !value.trim().is_empty() => Ok(Some(value.trim().to_owned())),
        Ok(_) | Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            anyhow::bail!("{ACCESS_KEY_ENV} is not valid Unicode")
        }
    }
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::protocol::{InitializeResult, PROTOCOL_VERSION};
    use serde_json::json;
    use std::io::Cursor;

    #[test]
    fn client_enforces_credentials_and_tls_on_remote_servers() {
        assert!(McpHttpClient::new("http://127.0.0.1:8081", None, false).is_ok());
        assert!(McpHttpClient::new("http://localhost:8081", None, true).is_ok());
        assert!(McpHttpClient::new("https://keeppeek.example.com", None, false).is_err());
        assert!(
            McpHttpClient::new(
                "http://keeppeek.example.com:8081",
                Some("550e8400-e29b-41d4-a716-446655440000".to_owned()),
                false
            )
            .is_err()
        );
        assert!(
            McpHttpClient::new(
                "https://keeppeek.example.com",
                Some("550e8400-e29b-41d4-a716-446655440000".to_owned()),
                true
            )
            .is_ok()
        );
    }

    #[test]
    fn stdio_loop_frames_initialize_notification_and_errors_cleanly() {
        let input = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-03-26\"}}\n",
            "\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
            "not-json\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n"
        );
        let mut output = Vec::new();
        serve_stdio_loop(Cursor::new(input.as_bytes()), &mut output, |line| {
            let value: Value = serde_json::from_str(line).unwrap();
            match value["method"].as_str() {
                Some("initialize") => {
                    let result =
                        serde_json::to_value(InitializeResult::keeppeek(false, false)).unwrap();
                    let response = JsonRpcResponse::success(value["id"].clone(), result);
                    Ok(Some(serde_json::to_string_pretty(&response).unwrap()))
                }
                Some("notifications/initialized") => Ok(None),
                Some("ping") => Ok(Some(
                    serde_json::to_string(&JsonRpcResponse::success(
                        value["id"].clone(),
                        json!({}),
                    ))
                    .unwrap(),
                )),
                _ => Err("unexpected method".to_owned()),
            }
        })
        .unwrap();

        let text = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);

        let first: JsonRpcResponse = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first.id, json!(1));
        assert_eq!(
            first.result.unwrap()["protocolVersion"],
            json!(PROTOCOL_VERSION)
        );

        let second: JsonRpcResponse = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second.error.unwrap().code, ERROR_PARSE);

        let third: JsonRpcResponse = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(third.id, json!(2));
        assert_eq!(third.result, Some(json!({})));
    }

    fn canonical_skill_markdown(dir_name: &str) -> String {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("skills")
                .join(dir_name)
                .join(skill::SKILL_FILE_NAME),
        )
        .unwrap()
    }

    #[test]
    fn skill_cli_command_renders_default_list_and_named_output() {
        let mut default_out = Vec::new();
        execute_skill_command(None, false, None, &mut default_out).unwrap();
        assert_eq!(
            String::from_utf8(default_out).unwrap(),
            canonical_skill_markdown("keeppeek-mcp")
        );

        let mut list_out = Vec::new();
        execute_skill_command(None, true, None, &mut list_out).unwrap();
        let list_text = String::from_utf8(list_out).unwrap();
        assert!(list_text.contains("keeppeek-mcp\t"));
        assert!(list_text.contains("keeppeek-camera-debug\t"));
        assert_eq!(list_text.lines().count(), 2);

        let canonical_debug = canonical_skill_markdown("keeppeek-camera-debug");
        let mut debug_out = Vec::new();
        execute_skill_command(Some("camera-debug"), false, None, &mut debug_out).unwrap();
        assert_eq!(String::from_utf8(debug_out).unwrap(), canonical_debug);

        let temp_dir =
            std::env::temp_dir().join(format!("keeppeek-stdio-skill-{}", uuid::Uuid::new_v4()));
        let mut write_out = Vec::new();
        execute_skill_command(
            Some("keeppeek-camera-debug"),
            false,
            Some(&temp_dir),
            &mut write_out,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(temp_dir.join(skill::SKILL_FILE_NAME)).unwrap(),
            canonical_debug
        );
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn skill_cli_command_rejects_conflicts_and_unknown_names() {
        use clap::Parser;

        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            mcp: McpArgs,
        }

        let mut conflict_out = Vec::new();
        assert!(
            execute_skill_command(None, true, Some(Path::new("out")), &mut conflict_out).is_err()
        );
        assert!(
            execute_skill_command(Some("camera-debug"), true, None, &mut conflict_out).is_err()
        );
        assert!(
            execute_skill_command(Some("nonexistent"), false, None, &mut conflict_out).is_err()
        );

        assert!(TestCli::try_parse_from(["keeppeek-mcp", "skill", "--list"]).is_ok());
        assert!(TestCli::try_parse_from(["keeppeek-mcp", "skill", "camera-debug"]).is_ok());
        assert!(
            TestCli::try_parse_from([
                "keeppeek-mcp",
                "skill",
                "keeppeek-camera-debug",
                "--output",
                "skills/keeppeek-camera-debug"
            ])
            .is_ok()
        );
        assert!(
            TestCli::try_parse_from(["keeppeek-mcp", "skill", "--list", "--output", "out"])
                .is_err()
        );
        assert!(
            TestCli::try_parse_from(["keeppeek-mcp", "skill", "--list", "camera-debug"]).is_err()
        );
    }

    #[test]
    fn cli_parses_pull_capture_subcommand() {
        use clap::Parser;

        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            mcp: McpArgs,
        }

        let pull = TestCli::try_parse_from([
            "keeppeek-mcp",
            "--server",
            "https://nvr.example.com",
            "pull-capture",
            "cap-123",
            "--output",
            "debug-out",
            "--artifact",
            "traces.jsonl",
            "--artifact",
            "capture.pcapng",
        ])
        .unwrap();
        match pull.mcp.command.unwrap() {
            McpCommand::PullCapture {
                capture_id,
                output,
                artifacts,
            } => {
                assert_eq!(capture_id, "cap-123");
                assert_eq!(output, PathBuf::from("debug-out"));
                assert_eq!(artifacts, vec!["traces.jsonl", "capture.pcapng"]);
            }
            _ => panic!("expected PullCapture"),
        }
    }

    #[test]
    fn cli_parses_logs_subcommand() {
        use clap::Parser;

        #[derive(Parser)]
        struct TestCli {
            #[command(flatten)]
            mcp: McpArgs,
        }

        let logs = TestCli::try_parse_from([
            "keeppeek-mcp",
            "logs",
            "--level",
            "debug",
            "--target",
            "keeppeek::rtsp",
            "--camera",
            "cam-1",
            "--query",
            "describe",
            "--filter",
            "retina=trace",
            "--follow",
            "--limit",
            "50",
        ])
        .unwrap();
        match logs.mcp.command.unwrap() {
            McpCommand::Logs {
                level,
                target,
                camera,
                query,
                filter,
                follow,
                limit,
            } => {
                assert_eq!(level, Some(LogLevelArg::Debug));
                assert_eq!(target.as_deref(), Some("keeppeek::rtsp"));
                assert_eq!(camera.as_deref(), Some("cam-1"));
                assert_eq!(query.as_deref(), Some("describe"));
                assert_eq!(filter.as_deref(), Some("retina=trace"));
                assert!(follow);
                assert_eq!(limit, 50);
            }
            _ => panic!("expected Logs"),
        }
    }

    fn spawn_mock_http_server<F>(
        max_requests: usize,
        handler: F,
    ) -> (String, std::thread::JoinHandle<()>)
    where
        F: Fn(String, String, Vec<(String, String)>, String) -> (u16, String, String)
            + Send
            + 'static,
    {
        use std::io::Read;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            for _ in 0..max_requests {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).unwrap() == 0 {
                    continue;
                }
                let mut parts = request_line.split_whitespace();
                let method = parts.next().unwrap_or("").to_owned();
                let path = parts.next().unwrap_or("").to_owned();
                let mut headers = Vec::new();
                let mut content_len = 0_usize;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let trimmed = line.trim_end_matches(['\r', '\n']);
                    if trimmed.is_empty() {
                        break;
                    }
                    if let Some((k, v)) = trimmed.split_once(':') {
                        if k.eq_ignore_ascii_case("content-length") {
                            content_len = v.trim().parse().unwrap_or(0);
                        }
                        headers.push((k.trim().to_owned(), v.trim().to_owned()));
                    }
                }
                let mut body_bytes = vec![0_u8; content_len];
                if content_len > 0 {
                    reader.read_exact(&mut body_bytes).unwrap();
                }
                let body = String::from_utf8(body_bytes).unwrap();
                let (status, content_type, resp_body) = handler(method, path, headers, body);
                let header = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    resp_body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(resp_body.as_bytes());
                let _ = stream.flush();
            }
        });
        (format!("http://{addr}"), handle)
    }

    fn mock_mcp_json_reply(id: Value, sc: Value) -> (u16, String, String) {
        let reply = JsonRpcResponse::success(
            id,
            serde_json::to_value(crate::mcp::protocol::ToolCallResult::json(sc)).unwrap(),
        );
        (
            200,
            "application/json".to_owned(),
            serde_json::to_string(&reply).unwrap(),
        )
    }

    fn mock_artifact_chunk_json(
        cap_id: &str,
        artifact: &str,
        slice: &[u8],
        offset: usize,
        total_bytes: usize,
        sha256: &str,
        encoding: &str,
    ) -> Value {
        let data = match encoding {
            "utf8" => std::str::from_utf8(slice).unwrap().to_owned(),
            _ => BASE64.encode(slice),
        };
        let next_offset = offset + slice.len();
        json!({
            "action": "read_artifact",
            "capture_id": cap_id,
            "artifact": artifact,
            "offset": offset,
            "length": slice.len(),
            "total_bytes": total_bytes,
            "sha256": sha256,
            "eof": next_offset >= total_bytes,
            "next_offset": next_offset,
            "encoding": encoding,
            "data": data
        })
    }

    fn spawn_pull_mock_server() -> (String, std::thread::JoinHandle<()>) {
        let trace_bytes = b"{\"line\":1}\n{\"line\":2}\n".to_vec();
        let bin_bytes: Vec<u8> = vec![0x00, 0x01, 0xFE, 0xFF, 0x42, 0x43];
        let trace_sha = digest_hex(&Sha256::digest(&trace_bytes));
        let bin_sha = digest_hex(&Sha256::digest(&bin_bytes));
        let corrupt_sha = "0".repeat(64);
        let req_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        spawn_mock_http_server(5, move |_method, _path, _headers, body| {
            let rpc: Value = serde_json::from_str(&body).unwrap();
            let args = &rpc["params"]["arguments"];
            let n = req_count.fetch_add(1, Ordering::Relaxed);
            let sc = match args["action"].as_str().unwrap() {
                "list_captures" => json!({
                    "action": "list_captures",
                    "captures": [{ "capture_id": "cap-pull-1", "files": ["traces.jsonl", "capture.pcapng"] }]
                }),
                "read_artifact" => {
                    let artifact = args["artifact"].as_str().unwrap();
                    let offset = args["offset"].as_u64().unwrap() as usize;
                    if artifact == "traces.jsonl" {
                        let end = if offset == 0 { 11 } else { trace_bytes.len() };
                        mock_artifact_chunk_json(
                            "cap-pull-1",
                            artifact,
                            &trace_bytes[offset..end],
                            offset,
                            trace_bytes.len(),
                            &trace_sha,
                            "utf8",
                        )
                    } else {
                        let sha = if n == 4 { &corrupt_sha } else { &bin_sha };
                        mock_artifact_chunk_json(
                            "cap-pull-1",
                            artifact,
                            &bin_bytes,
                            0,
                            bin_bytes.len(),
                            sha,
                            "base64",
                        )
                    }
                }
                other => panic!("unexpected action {other}"),
            };
            mock_mcp_json_reply(rpc["id"].clone(), sc)
        })
    }

    #[test]
    fn pull_capture_downloads_chunks_verifies_sha256_and_blocks_traversal() {
        let (origin, server) = spawn_pull_mock_server();
        let client = McpHttpClient::new(&origin, None, false).unwrap();
        let out_dir =
            std::env::temp_dir().join(format!("keeppeek-pull-test-{}", uuid::Uuid::new_v4()));
        let mut out_log = Vec::new();
        execute_pull_capture(&client, "cap-pull-1", &out_dir, &[], &mut out_log).unwrap();
        assert_eq!(
            std::fs::read(out_dir.join("traces.jsonl")).unwrap(),
            b"{\"line\":1}\n{\"line\":2}\n"
        );
        assert_eq!(
            std::fs::read(out_dir.join("capture.pcapng")).unwrap(),
            vec![0x00, 0x01, 0xFE, 0xFF, 0x42, 0x43]
        );

        let traversal_err = execute_pull_capture(
            &client,
            "cap-pull-1",
            &out_dir,
            &["../escape.txt".to_owned()],
            &mut out_log,
        )
        .unwrap_err();
        assert!(traversal_err.to_string().contains("disallowed"));

        let mismatch_err = execute_pull_capture(
            &client,
            "cap-pull-1",
            &out_dir,
            &["capture.pcapng".to_owned()],
            &mut out_log,
        )
        .unwrap_err();
        assert!(mismatch_err.to_string().contains("SHA-256 mismatch"));

        let _ = server.join();
        let _ = std::fs::remove_dir_all(&out_dir);
    }

    #[test]
    fn pull_capture_downloads_131k_control_byte_base64_chunks() {
        let payload = vec![0x01_u8; 131_072];
        let payload_sha = digest_hex(&Sha256::digest(&payload));
        let payload_srv = payload.clone();
        let (origin, server) = spawn_mock_http_server(2, move |_m, _p, _h, body| {
            let rpc: Value = serde_json::from_str(&body).unwrap();
            let args = &rpc["params"]["arguments"];
            assert_eq!(args["encoding"], "auto");
            let offset = args["offset"].as_u64().unwrap() as usize;
            let len = args["length"].as_u64().unwrap() as usize;
            let end = offset.saturating_add(len).min(payload_srv.len());
            let sc = mock_artifact_chunk_json(
                "cap-ctrl",
                "ctrl.bin",
                &payload_srv[offset..end],
                offset,
                payload_srv.len(),
                &payload_sha,
                "base64",
            );
            mock_mcp_json_reply(rpc["id"].clone(), sc)
        });
        let client = McpHttpClient::new(&origin, None, false).unwrap();
        let out_dir =
            std::env::temp_dir().join(format!("keeppeek-pull-ctrl-{}", uuid::Uuid::new_v4()));
        let mut out_log = Vec::new();
        execute_pull_capture(
            &client,
            "cap-ctrl",
            &out_dir,
            &["ctrl.bin".to_owned()],
            &mut out_log,
        )
        .unwrap();
        assert_eq!(std::fs::read(out_dir.join("ctrl.bin")).unwrap(), payload);
        let _ = server.join();
        let _ = std::fs::remove_dir_all(&out_dir);
    }

    fn sample_log_fixture() -> (LogEntry, LogSnapshot, String) {
        let entry_match = LogEntry {
            sequence: 10,
            timestamp_ms: 1_000,
            level: LogLevel::Debug,
            target: "keeppeek::rtsp".to_owned(),
            message: "describe ok".to_owned(),
            fields: std::collections::BTreeMap::from([(
                "camera_id".to_owned(),
                Value::String("cam-1".to_owned()),
            )]),
            file: None,
            line: None,
        };
        let entry_other = LogEntry {
            sequence: 11,
            timestamp_ms: 1_001,
            level: LogLevel::Trace,
            target: "keeppeek::server".to_owned(),
            message: "noise".to_owned(),
            fields: std::collections::BTreeMap::from([(
                "camera_id".to_owned(),
                Value::String("cam-2".to_owned()),
            )]),
            file: None,
            line: None,
        };
        let snapshot = LogSnapshot {
            entries: vec![entry_match.clone(), entry_other],
            oldest_sequence: Some(10),
            newest_sequence: Some(11),
            truncated: false,
            stats: crate::logging::LogBufferStats {
                entry_count: 2,
                byte_count: 100,
                evicted_entries: 0,
                max_entries: 100,
                max_bytes: 1024,
                active_streams: 0,
                max_streams: 8,
            },
            generation: Some("gen-1".to_owned()),
        };
        let sse_body = format!(
            ": connected\n\n: keep-alive\n\nid: 10\nevent: log\ndata: {}\n\n",
            serde_json::to_string(&entry_match).unwrap()
        );
        (entry_match, snapshot, sse_body)
    }

    fn spawn_mock_logs_server(
        max_requests: usize,
        applied_filters: Arc<Mutex<Vec<String>>>,
    ) -> (String, std::thread::JoinHandle<()>) {
        let (_, snapshot, sse_body) = sample_log_fixture();
        spawn_mock_http_server(max_requests, move |method, path, _headers, body| {
            if method == "POST" && path == "/mcp" {
                let rpc: Value = serde_json::from_str(&body).unwrap();
                let sc = match rpc["params"]["name"].as_str().unwrap() {
                    "get_logging_settings" => json!({
                        "base_filter": "info,keeppeek=debug",
                        "active_filter": "info,keeppeek=debug,retina=trace"
                    }),
                    "set_logging_filter" => {
                        let f = rpc["params"]["arguments"]["filter"]
                            .as_str()
                            .unwrap()
                            .to_owned();
                        applied_filters.lock().unwrap().push(f.clone());
                        json!({ "active_filter": f })
                    }
                    other => panic!("unexpected tool {other}"),
                };
                mock_mcp_json_reply(rpc["id"].clone(), sc)
            } else if method == "GET" && path == "/logs/snapshot" {
                (
                    200,
                    "application/json".to_owned(),
                    serde_json::to_string(&snapshot).unwrap(),
                )
            } else if method == "GET" && path.starts_with("/logs?") {
                (
                    200,
                    "text/event-stream; charset=utf-8".to_owned(),
                    sse_body.clone(),
                )
            } else {
                (404, "text/plain".to_owned(), "not found".to_owned())
            }
        })
    }

    #[test]
    fn logs_command_fetches_snapshot_and_restores_base_filter() {
        let applied_filters = Arc::new(Mutex::new(Vec::<String>::new()));
        let (origin, server) = spawn_mock_logs_server(4, applied_filters.clone());
        let client = McpHttpClient::new(&origin, None, false).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let mut snap_out = Vec::new();
        execute_logs_command(
            &client,
            &LogsCommandOptions {
                level: Some(LogLevel::Debug),
                target: Some("rtsp".to_owned()),
                camera: Some("cam-1".to_owned()),
                query: Some("describe".to_owned()),
                filter: Some("retina=trace".to_owned()),
                follow: false,
                limit: 100,
            },
            &stop,
            false,
            &mut snap_out,
        )
        .unwrap();
        let snap_text = String::from_utf8(snap_out).unwrap();
        assert_eq!(snap_text.lines().count(), 1);
        assert!(snap_text.contains("\"camera_id\":\"cam-1\""));
        assert_eq!(
            *applied_filters.lock().unwrap(),
            vec!["retina=trace", "info,keeppeek=debug"]
        );
        let _ = server.join();
    }

    #[test]
    fn logs_command_streams_sse_and_restores_base_filter() {
        let applied_filters = Arc::new(Mutex::new(Vec::<String>::new()));
        let (origin, server) = spawn_mock_logs_server(4, applied_filters.clone());
        let client = McpHttpClient::new(&origin, None, false).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let mut sse_out = Vec::new();
        execute_logs_command(
            &client,
            &LogsCommandOptions {
                level: Some(LogLevel::Debug),
                target: None,
                camera: Some("cam-1".to_owned()),
                query: None,
                filter: Some("keeppeek::rtsp=trace".to_owned()),
                follow: true,
                limit: 25,
            },
            &stop,
            false,
            &mut sse_out,
        )
        .unwrap();
        let sse_text = String::from_utf8(sse_out).unwrap();
        assert_eq!(sse_text.lines().count(), 1);
        assert!(sse_text.contains("\"describe ok\""));
        assert_eq!(
            *applied_filters.lock().unwrap(),
            vec!["keeppeek::rtsp=trace", "info,keeppeek=debug"]
        );
        let _ = server.join();
    }

    #[test]
    fn remote_filter_guard_preserves_base_filter_during_active_trace_boost() {
        let dir = std::env::temp_dir().join(format!("keeppeek-rfg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let filter_path = dir.join("log-filter");
        let (logging, _dispatch) = crate::logging::LoggingService::for_test(
            crate::logging::LogFilterFile::new(filter_path.clone()),
            "info,keeppeek=debug",
        );
        logging.update_filter("info,keeppeek=debug").unwrap();
        let trace_boost = logging.acquire_trace_filter_boost("retina=trace").unwrap();
        let srv_logging = logging.clone();
        let (origin, server) = spawn_mock_http_server(3, move |_m, _p, _h, body| {
            let rpc: Value = serde_json::from_str(&body).unwrap();
            let sc = match rpc["params"]["name"].as_str().unwrap() {
                "get_logging_settings" => json!({
                    "base_filter": srv_logging.base_filter(),
                    "active_filter": srv_logging.active_filter()
                }),
                "set_logging_filter" => {
                    let f = rpc["params"]["arguments"]["filter"].as_str().unwrap();
                    srv_logging.update_filter(f).unwrap();
                    json!({ "active_filter": srv_logging.active_filter() })
                }
                other => panic!("unexpected tool {other}"),
            };
            mock_mcp_json_reply(rpc["id"].clone(), sc)
        });
        let client = McpHttpClient::new(&origin, None, false).unwrap();
        {
            let _remote = RemoteFilterGuard::acquire(&client, "keeppeek::rtsp=trace").unwrap();
            assert!(logging.active_filter().contains("retina=trace"));
        }
        drop(trace_boost);
        assert_eq!(logging.base_filter(), "info,keeppeek=debug");
        assert_eq!(logging.active_filter(), "info,keeppeek=debug");
        assert_eq!(
            std::fs::read_to_string(&filter_path).unwrap().trim(),
            "info,keeppeek=debug"
        );
        let _ = server.join();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pull_capture_rejects_oversized_mismatched_and_non_advancing_chunks() {
        let dir =
            std::env::temp_dir().join(format!("keeppeek-pull-bounds-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let empty_sha = digest_hex(&Sha256::digest(b""));
        let one_byte_sha = digest_hex(&Sha256::digest(b"ab"));

        let oversized_total = MAX_PULL_ARTIFACT_BYTES + 1;
        let (oversized_url, oversized_srv) = spawn_mock_http_server(1, move |_m, _p, _h, body| {
            let rpc: Value = serde_json::from_str(&body).unwrap();
            let mut chunk =
                mock_artifact_chunk_json("cap-1", "big.bin", b"", 0, 0, &empty_sha, "utf8");
            chunk["total_bytes"] = json!(oversized_total);
            chunk["eof"] = json!(false);
            mock_mcp_json_reply(rpc["id"].clone(), chunk)
        });
        let client = McpHttpClient::new(&oversized_url, None, false).unwrap();
        let mut out = File::create(dir.join("oversized.tmp")).unwrap();
        let err = stream_artifact_chunks(&client, "cap-1", "big.bin", &mut out).unwrap_err();
        assert!(err.to_string().contains("exceeds limit"));
        let _ = oversized_srv.join();

        let mismatch_sha = one_byte_sha.clone();
        let (mismatch_url, mismatch_srv) = spawn_mock_http_server(2, move |_m, _p, _h, body| {
            let rpc: Value = serde_json::from_str(&body).unwrap();
            let offset = rpc["params"]["arguments"]["offset"].as_u64().unwrap() as usize;
            let sc = if offset == 0 {
                mock_artifact_chunk_json("cap-1", "a.bin", b"a", 0, 2, &mismatch_sha, "utf8")
            } else {
                mock_artifact_chunk_json("cap-1", "a.bin", b"b", 1, 99, &mismatch_sha, "utf8")
            };
            mock_mcp_json_reply(rpc["id"].clone(), sc)
        });
        let client = McpHttpClient::new(&mismatch_url, None, false).unwrap();
        let mut out = File::create(dir.join("mismatch.tmp")).unwrap();
        let err = stream_artifact_chunks(&client, "cap-1", "a.bin", &mut out).unwrap_err();
        assert!(err.to_string().contains("total_bytes changed"));
        let _ = mismatch_srv.join();

        let nonadv_sha = one_byte_sha;
        let (nonadv_url, nonadv_srv) = spawn_mock_http_server(1, move |_m, _p, _h, body| {
            let rpc: Value = serde_json::from_str(&body).unwrap();
            let mut chunk =
                mock_artifact_chunk_json("cap-1", "a.bin", b"a", 0, 2, &nonadv_sha, "utf8");
            chunk["next_offset"] = json!(0);
            mock_mcp_json_reply(rpc["id"].clone(), chunk)
        });
        let client = McpHttpClient::new(&nonadv_url, None, false).unwrap();
        let mut out = File::create(dir.join("nonadv.tmp")).unwrap();
        let err = stream_artifact_chunks(&client, "cap-1", "a.bin", &mut out).unwrap_err();
        assert!(err.to_string().contains("next_offset mismatch"));
        let _ = nonadv_srv.join();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pull_capture_enforces_chunk_cap_and_cleans_failed_persist() {
        let dir = std::env::temp_dir().join(format!("keeppeek-pull-cap-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let cap_sha = digest_hex(&Sha256::digest(b"abcd"));
        let (cap_url, cap_srv) = spawn_mock_http_server(2, move |_m, _p, _h, body| {
            let rpc: Value = serde_json::from_str(&body).unwrap();
            let offset = rpc["params"]["arguments"]["offset"].as_u64().unwrap() as usize;
            let sc = mock_artifact_chunk_json(
                "cap-1",
                "a.bin",
                &b"abcd"[offset..offset + 1],
                offset,
                4,
                &cap_sha,
                "utf8",
            );
            mock_mcp_json_reply(rpc["id"].clone(), sc)
        });
        let client = McpHttpClient::new(&cap_url, None, false).unwrap();
        let mut out = File::create(dir.join("cap.tmp")).unwrap();
        let err =
            stream_artifact_chunks_bounded(&client, "cap-1", "a.bin", &mut out, 2).unwrap_err();
        assert!(err.to_string().contains("exceeded maximum chunk count"));
        let _ = cap_srv.join();

        let temp_path = dir.join(".pull-fail.tmp");
        let target_dir = dir.join("existing-dir-target");
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::write(target_dir.join("occupied"), b"x").unwrap();
        std::fs::write(&temp_path, b"payload").unwrap();
        let guard = TempFileGuard::new(temp_path.clone());
        assert!(guard.persist(&target_dir).is_err());
        assert!(!temp_path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remote_filter_guard_restore_now_serializes_concurrent_callers() {
        let (tx_started, rx_started) = std::sync::mpsc::channel();
        let (tx_release, rx_release) = std::sync::mpsc::channel();
        let rx_release = Arc::new(Mutex::new(rx_release));
        let (origin, server) = spawn_mock_http_server(1, move |_m, _p, _h, body| {
            let rpc: Value = serde_json::from_str(&body).unwrap();
            let _ = tx_started.send(());
            let _ = rx_release.lock().unwrap().recv();
            mock_mcp_json_reply(rpc["id"].clone(), json!({ "active_filter": "info" }))
        });
        let client = McpHttpClient::new(&origin, None, false).unwrap();
        let state = Arc::new(Mutex::new(Some((client, "info".to_owned()))));
        let state_t1 = state.clone();
        let t1 = std::thread::spawn(move || RemoteFilterGuard::restore_now(&state_t1));
        rx_started.recv().unwrap();

        let (tx_t2_done, rx_t2_done) = std::sync::mpsc::channel();
        let state_t2 = state;
        let t2 = std::thread::spawn(move || {
            RemoteFilterGuard::restore_now(&state_t2);
            let _ = tx_t2_done.send(());
        });
        assert!(rx_t2_done.recv_timeout(Duration::from_millis(80)).is_err());

        tx_release.send(()).unwrap();
        t1.join().unwrap();
        rx_t2_done.recv_timeout(Duration::from_secs(2)).unwrap();
        t2.join().unwrap();
        let _ = server.join();
    }

    #[test]
    fn stream_sse_logs_accounts_for_newline_and_discards_overflow_cleanly() {
        let (_, _, ok_sse) = sample_log_fixture();
        let matcher = LogFilterMatcher::from_options(&LogsCommandOptions {
            level: None,
            target: None,
            camera: None,
            query: None,
            filter: None,
            follow: true,
            limit: 10,
        });
        let stop = AtomicBool::new(false);
        let half = "x".repeat(MAX_SSE_DATA_BYTES / 2);
        let payload = format!("event: log\ndata: {half}\ndata: {half}\ndata: tail\n\n{ok_sse}");
        let mut cursor = io::Cursor::new(payload.into_bytes());
        let mut out = Vec::new();
        stream_sse_logs(&mut cursor, &matcher, &mut out, &stop).unwrap();
        let rendered = String::from_utf8(out).unwrap();
        assert_eq!(rendered.lines().count(), 1);
        assert!(rendered.contains("\"describe ok\""));
    }
}
