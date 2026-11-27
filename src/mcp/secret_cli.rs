//! Command-line handlers for managing KeepPeek `secrets.toml` entries.
//!
//! This module defines the Clap arguments and execution helpers for
//! `keeppeek secret set`, `keeppeek secret list`, and `keeppeek secret remove`.
//! Secret values are read only from `--stdin` or a no-echo interactive terminal
//! prompt and are never accepted as command-line arguments.

use crate::config::{
    self,
    secrets_store::{
        SECRET_VALUE_BYTES_MAX, list_secret_keys, remove_secret_entry, set_secret_entry,
        validate_secret_key, validate_secret_value,
    },
};
use clap::{Args, Subcommand};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    terminal,
};
use std::{
    fmt,
    io::{self, IsTerminal as _, Read, Write},
    path::PathBuf,
    time::Duration,
};
use url::{Host, Url};
use zeroize::Zeroizing;

const SERVER_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Command-line arguments for `keeppeek secret`.
#[derive(Debug, Args)]
#[command(name = "keeppeek secret")]
pub struct SecretArgs {
    /// Use a specific server configuration file to locate its sibling `secrets.toml`.
    #[arg(short = 'c', long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Optional local or HTTPS server URL to check for live reload availability.
    #[arg(long, global = true, value_name = "URL")]
    pub server: Option<String>,
    /// Secret management subcommand to execute.
    #[command(subcommand)]
    pub command: SecretCommand,
}

/// Supported subcommands under `keeppeek secret`.
#[derive(Debug, Subcommand)]
pub enum SecretCommand {
    /// Store or update a secret value from `--stdin` or a hidden terminal prompt.
    Set {
        /// Secret key name matching `^[A-Z_][A-Z0-9_]*$` (1 to 128 bytes).
        key: String,
        /// Read the secret value from standard input instead of an interactive prompt.
        #[arg(long)]
        stdin: bool,
    },
    /// Print sorted secret key names without exposing any secret values.
    List,
    /// Remove a secret entry from `secrets.toml`.
    Remove {
        /// Secret key name to remove.
        key: String,
    },
}

/// Errors returned by `keeppeek secret` CLI commands.
#[derive(Debug)]
pub enum SecretCliError {
    /// Invalid arguments, key syntax, value bounds, or non-interactive usage.
    Usage(String),
    /// Requested secret key was not found during removal.
    NotFound(String),
    /// Interactive input was cancelled by the user.
    Cancelled,
    /// Filesystem, terminal, or output I/O failure.
    Io(String),
}

impl SecretCliError {
    /// Returns the process exit code for this CLI error.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => 2,
            Self::Cancelled => 130,
            Self::NotFound(_) | Self::Io(_) => 1,
        }
    }
}

impl fmt::Display for SecretCliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) | Self::NotFound(message) | Self::Io(message) => {
                formatter.write_str(message)
            }
            Self::Cancelled => formatter.write_str("secret entry cancelled"),
        }
    }
}

impl std::error::Error for SecretCliError {}

/// Executes `keeppeek secret` using standard I/O streams.
///
/// # Errors
///
/// Returns a [`SecretCliError`] when arguments are invalid, interactive input
/// is cancelled, a removed key does not exist, or writing `secrets.toml` fails.
pub fn run(args: SecretArgs) -> Result<(), SecretCliError> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let stderr = io::stderr();
    let stdin_is_tty = stdin.is_terminal();
    run_with_io(
        args,
        stdin.lock(),
        stdin_is_tty,
        stdout.lock(),
        stderr.lock(),
        read_secret_from_tty,
    )
}

fn run_with_io<R, WOut, WErr, F>(
    args: SecretArgs,
    stdin: R,
    stdin_is_tty: bool,
    mut stdout: WOut,
    mut stderr: WErr,
    read_tty: F,
) -> Result<(), SecretCliError>
where
    R: Read,
    WOut: Write,
    WErr: Write,
    F: FnOnce(&str, &mut WErr) -> Result<Zeroizing<String>, SecretCliError>,
{
    let secrets_path = resolve_secrets_path(args.config);
    let server_url = args
        .server
        .as_deref()
        .map(validate_server_url)
        .transpose()?;

    match args.command {
        SecretCommand::Set { key, stdin: pipe } => run_set_command(
            &secrets_path,
            server_url.as_ref(),
            &key,
            pipe,
            stdin,
            stdin_is_tty,
            &mut stdout,
            &mut stderr,
            read_tty,
        ),
        SecretCommand::List => run_list_command(&secrets_path, &mut stdout),
        SecretCommand::Remove { key } => run_remove_command(&secrets_path, &key, &mut stdout),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "set helper accepts explicit I/O streams for deterministic CLI testing"
)]
fn run_set_command<R, WOut, WErr, F>(
    secrets_path: &std::path::Path,
    server_url: Option<&Url>,
    key: &str,
    pipe: bool,
    stdin: R,
    stdin_is_tty: bool,
    stdout: &mut WOut,
    stderr: &mut WErr,
    read_tty: F,
) -> Result<(), SecretCliError>
where
    R: Read,
    WOut: Write,
    WErr: Write,
    F: FnOnce(&str, &mut WErr) -> Result<Zeroizing<String>, SecretCliError>,
{
    validate_secret_key(key).map_err(|error| SecretCliError::Usage(error.to_string()))?;
    let secret = if pipe {
        read_secret_from_reader(stdin)?
    } else if stdin_is_tty {
        read_tty(key, stderr)?
    } else {
        return Err(SecretCliError::Usage(
            "standard input is not a terminal; pass --stdin to read a secret from a pipe"
                .to_owned(),
        ));
    };
    validate_secret_value(&secret).map_err(|error| SecretCliError::Usage(error.to_string()))?;
    set_secret_entry(secrets_path, key, &secret)
        .map_err(|error| SecretCliError::Io(error.to_string()))?;
    let live_note = server_url.map_or("live reload applies automatically on next lookup", |url| {
        if probe_server_reachable(url) {
            "running server detected; live reload applies without restart"
        } else {
            "server not reachable; saved locally for next start"
        }
    });
    writeln!(stdout, "Saved secret {key} ({live_note}).")
        .map_err(|error| SecretCliError::Io(format!("unable to write output: {error}")))
}

fn run_list_command(
    secrets_path: &std::path::Path,
    stdout: &mut impl Write,
) -> Result<(), SecretCliError> {
    let keys =
        list_secret_keys(secrets_path).map_err(|error| SecretCliError::Io(error.to_string()))?;
    for key in keys {
        writeln!(stdout, "{key}")
            .map_err(|error| SecretCliError::Io(format!("unable to write output: {error}")))?;
    }
    Ok(())
}

fn run_remove_command(
    secrets_path: &std::path::Path,
    key: &str,
    stdout: &mut impl Write,
) -> Result<(), SecretCliError> {
    validate_secret_key(key).map_err(|error| SecretCliError::Usage(error.to_string()))?;
    let removed = remove_secret_entry(secrets_path, key)
        .map_err(|error| SecretCliError::Io(error.to_string()))?;
    if !removed {
        return Err(SecretCliError::NotFound(format!(
            "secret key '{key}' was not found"
        )));
    }
    writeln!(stdout, "Removed secret {key}.")
        .map_err(|error| SecretCliError::Io(format!("unable to write output: {error}")))
}

fn resolve_secrets_path(config_override: Option<PathBuf>) -> PathBuf {
    let config_path = config_override.unwrap_or_else(config::config_path);
    config::secrets_path(&config_path)
}

fn read_secret_from_reader(reader: impl Read) -> Result<Zeroizing<String>, SecretCliError> {
    // Allow up to 2 extra bytes for a single trailing CRLF before enforcing the 4 KiB cap.
    let read_limit = (SECRET_VALUE_BYTES_MAX + 3) as u64;
    let mut raw = Zeroizing::new(Vec::new());
    reader
        .take(read_limit)
        .read_to_end(&mut raw)
        .map_err(|error| {
            SecretCliError::Io(format!("unable to read secret from stdin: {error}"))
        })?;
    let mut text = Zeroizing::new(
        String::from_utf8(std::mem::take(&mut *raw))
            .map_err(|_| SecretCliError::Usage("secret value must be valid UTF-8".to_owned()))?,
    );
    strip_single_trailing_newline(&mut text);
    validate_secret_value(&text).map_err(|error| SecretCliError::Usage(error.to_string()))?;
    Ok(text)
}

fn strip_single_trailing_newline(text: &mut String) {
    if text.ends_with('\n') {
        text.pop();
        if text.ends_with('\r') {
            text.pop();
        }
    }
}

struct RawModeGuard;

impl RawModeGuard {
    fn acquire() -> Result<Self, SecretCliError> {
        terminal::enable_raw_mode().map_err(|error| {
            SecretCliError::Io(format!("unable to disable terminal echo: {error}"))
        })?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

fn read_secret_from_tty(
    key: &str,
    stderr: &mut impl Write,
) -> Result<Zeroizing<String>, SecretCliError> {
    write!(stderr, "Enter value for {key}: ")
        .and_then(|()| stderr.flush())
        .map_err(|error| SecretCliError::Io(format!("unable to write prompt: {error}")))?;
    let _raw_mode = RawModeGuard::acquire()?;
    let mut secret = Zeroizing::new(String::new());
    loop {
        let event = event::read().map_err(|error| {
            SecretCliError::Io(format!("unable to read terminal input: {error}"))
        })?;
        let Event::Key(key_event) = event else {
            continue;
        };
        if key_event.kind != KeyEventKind::Press {
            continue;
        }
        match key_event.code {
            KeyCode::Enter => {
                let _ = write!(stderr, "\r\n");
                let _ = stderr.flush();
                return Ok(secret);
            }
            KeyCode::Esc => {
                let _ = write!(stderr, "\r\n");
                let _ = stderr.flush();
                return Err(SecretCliError::Cancelled);
            }
            KeyCode::Char('c') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                let _ = write!(stderr, "\r\n");
                let _ = stderr.flush();
                return Err(SecretCliError::Cancelled);
            }
            KeyCode::Backspace => {
                secret.pop();
            }
            KeyCode::Char(character) => {
                if secret.len() + character.len_utf8() > SECRET_VALUE_BYTES_MAX {
                    let _ = write!(stderr, "\r\n");
                    let _ = stderr.flush();
                    return Err(SecretCliError::Usage(format!(
                        "secret value must not exceed {SECRET_VALUE_BYTES_MAX} bytes"
                    )));
                }
                secret.push(character);
            }
            _ => {}
        }
    }
}

fn validate_server_url(raw: &str) -> Result<Url, SecretCliError> {
    let url = Url::parse(raw)
        .map_err(|_| SecretCliError::Usage("server URL must be a valid HTTP(S) URL".to_owned()))?;
    if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
        return Err(SecretCliError::Usage(
            "server URL must use http or https with a host".to_owned(),
        ));
    }
    if url.scheme() == "http" && !is_loopback_host(&url) {
        return Err(SecretCliError::Usage(
            "remote server URLs must use https; http is allowed only on loopback".to_owned(),
        ));
    }
    Ok(url)
}

fn is_loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

fn probe_server_reachable(server_url: &Url) -> bool {
    let Ok(health_url) = server_url.join("/api/health") else {
        return false;
    };
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(SERVER_PROBE_TIMEOUT))
        .http_status_as_error(false)
        .build()
        .new_agent();
    let mut request = agent.get(health_url.as_str());
    if let Ok(access_key) = std::env::var("KEEPPEEK_ACCESS_KEY")
        && !access_key.is_empty()
    {
        request = request.header("Authorization", &format!("Bearer {access_key}"));
    }
    request
        .call()
        .is_ok_and(|response| response.status().as_u16() < 500)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::io::Cursor;

    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: TestSubcommand,
    }

    #[derive(Debug, Subcommand)]
    enum TestSubcommand {
        Secret(SecretArgs),
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let path = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn clap_rejects_secret_values_passed_as_positional_or_flag_arguments() {
        assert!(
            TestCli::try_parse_from([
                "keeppeek",
                "secret",
                "set",
                "CAM_PASS",
                "plaintext-value-must-fail",
            ])
            .is_err()
        );
        assert!(
            TestCli::try_parse_from([
                "keeppeek",
                "secret",
                "set",
                "CAM_PASS",
                "--value",
                "plaintext-value-must-fail",
            ])
            .is_err()
        );
        assert!(
            TestCli::try_parse_from([
                "keeppeek",
                "secret",
                "set",
                "CAM_PASS",
                "--config",
                "/tmp/config.toml",
                "--stdin",
            ])
            .is_ok()
        );
        assert!(
            TestCli::try_parse_from(["keeppeek", "secret", "list", "--config", "/tmp/config.toml"])
                .is_ok()
        );
        assert!(
            TestCli::try_parse_from([
                "keeppeek",
                "secret",
                "remove",
                "CAM_PASS",
                "--config",
                "/tmp/config.toml",
            ])
            .is_ok()
        );
    }

    fn invoke_piped(
        config_path: &std::path::Path,
        command: SecretCommand,
        input: &str,
    ) -> Result<(String, String), SecretCliError> {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_with_io(
            SecretArgs {
                config: Some(config_path.to_path_buf()),
                server: None,
                command,
            },
            Cursor::new(input.as_bytes()),
            false,
            &mut stdout,
            &mut stderr,
            |_, _| unreachable!("piped mode must not invoke tty prompt"),
        )?;
        Ok((
            String::from_utf8(stdout).unwrap(),
            String::from_utf8(stderr).unwrap(),
        ))
    }

    #[test]
    fn stdin_set_list_and_remove_never_leak_secret_value_to_output() {
        let dir = TempDir::new("keeppeek-secret-cli");
        let config_path = dir.0.join("config.toml");
        let sentinel = "CLI_SENTINEL_SECRET_99112233";

        let (set_out, set_err) = invoke_piped(
            &config_path,
            SecretCommand::Set {
                key: "FRONT_CAM_PASS".to_owned(),
                stdin: true,
            },
            &format!("{sentinel}\r\n"),
        )
        .unwrap();
        assert!(set_out.contains("FRONT_CAM_PASS"));
        assert!(!set_out.contains(sentinel));
        assert!(!set_err.contains(sentinel));

        let (listed, _) = invoke_piped(&config_path, SecretCommand::List, "").unwrap();
        assert_eq!(listed, "FRONT_CAM_PASS\n");
        assert!(!listed.contains(sentinel));

        let secrets_path = config::secrets_path(&config_path);
        let loaded = config::secrets_store::load_secrets_file(&secrets_path).unwrap();
        assert_eq!(
            config::secrets_store::resolve_loaded_reference("{secret:FRONT_CAM_PASS}", &loaded)
                .unwrap()
                .as_str(),
            sentinel
        );

        let (remove_out, _) = invoke_piped(
            &config_path,
            SecretCommand::Remove {
                key: "FRONT_CAM_PASS".to_owned(),
            },
            "",
        )
        .unwrap();
        assert!(remove_out.contains("FRONT_CAM_PASS"));
        assert!(
            config::secrets_store::list_secret_keys(&secrets_path)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn non_tty_set_without_stdin_flag_is_rejected_and_tty_prompt_works() {
        let dir = TempDir::new("keeppeek-secret-cli-tty");
        let config_path = dir.0.join("config.toml");

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let err = run_with_io(
            SecretArgs {
                config: Some(config_path.clone()),
                server: None,
                command: SecretCommand::Set {
                    key: "TTY_KEY".to_owned(),
                    stdin: false,
                },
            },
            Cursor::new(b"ignored".as_slice()),
            false,
            &mut stdout,
            &mut stderr,
            |_, _| unreachable!("non-tty must fail before prompt"),
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), 2);

        run_with_io(
            SecretArgs {
                config: Some(config_path.clone()),
                server: None,
                command: SecretCommand::Set {
                    key: "TTY_KEY".to_owned(),
                    stdin: false,
                },
            },
            Cursor::new(Vec::<u8>::new()),
            true,
            &mut stdout,
            &mut stderr,
            |_, _| Ok(Zeroizing::new("tty-hidden-secret".to_owned())),
        )
        .unwrap();
        assert_eq!(
            config::secrets_store::list_secret_keys(&config::secrets_path(&config_path)).unwrap(),
            vec!["TTY_KEY"]
        );
    }

    #[test]
    fn server_url_rejects_remote_plaintext_http() {
        assert!(validate_server_url("http://127.0.0.1:3000").is_ok());
        assert!(validate_server_url("http://localhost:3000").is_ok());
        assert!(validate_server_url("https://nvr.example.com").is_ok());
        assert!(validate_server_url("http://192.168.1.10:3000").is_err());
        assert!(validate_server_url("ftp://127.0.0.1").is_err());
    }
}
