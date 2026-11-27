//! Stdio bridge CLI for connecting MCP clients to a KeepPeek server.
//!
//! `keeppeek mcp` reads newline-delimited JSON-RPC 2.0 requests from `stdin`,
//! forwards them over HTTP(S) to `POST /mcp`, and writes single-line JSON-RPC
//! responses strictly to `stdout`. All diagnostics go to `stderr`.

use crate::{
    config,
    mcp::{
        protocol::{
            ERROR_INTERNAL, ERROR_INVALID_REQUEST, ERROR_PARSE, JsonRpcError, JsonRpcResponse,
            MAX_REQUEST_BODY_BYTES, READ_ONLY_HEADER,
        },
        skill,
    },
};
use clap::{Args, Subcommand};
use serde_json::Value;
use std::{
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use ureq::Agent;
use url::{Host, Url};

const STDIO_HTTP_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_STDIO_RESPONSE_BYTES: u64 = 2 * 1_024 * 1_024;
const ACCESS_KEY_ENV: &str = "KEEPPEEK_ACCESS_KEY";

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
}

/// HTTP client that forwards JSON-RPC requests to `POST /mcp`.
pub struct McpHttpClient {
    agent: Agent,
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
            endpoint: parsed,
            authorization: key.map(|token| format!("Bearer {token}")),
            read_only,
        })
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
}
