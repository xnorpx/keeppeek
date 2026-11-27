mod keeppeek_backup;

use clap::{Parser, Subcommand};
use keeppeek::{
    config,
    logging::initialize_global_logging,
    shutdown::{Restart, Shutdown, restart_current_process},
};
use tracing::info;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Parser)]
#[command(name = "keeppeek", version)]
struct Cli {
    /// Use a specific server configuration file.
    #[arg(short = 'c', long, value_name = "PATH")]
    config: Option<std::path::PathBuf>,
    /// Show stream statistics in an interactive terminal.
    #[arg(long, value_enum, default_value_t = keeppeek::tui::DashboardMode::Auto)]
    tui: keeppeek::tui::DashboardMode,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Export or apply configuration and secrets through the HTTP API.
    Config(keeppeek_backup::BackupArgs),
    /// Run the KeepPeek MCP stdio bridge or export bundled agent skills.
    Mcp(keeppeek::mcp::stdio::McpArgs),
    /// Manage private entries in secrets.toml without passing secret values on the command line.
    Secret(keeppeek::mcp::secret_cli::SecretArgs),
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(RunOutcome::Finished) => std::process::ExitCode::SUCCESS,
        Ok(RunOutcome::SupervisedRestart) => {
            std::process::ExitCode::from(keeppeek_supervisor::EXIT_CODE_RESTART_REQUESTED)
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::from(error.exit_code())
        }
    }
}

enum RunOutcome {
    Finished,
    SupervisedRestart,
}

enum MainError {
    Server(anyhow::Error),
    Backup(keeppeek_backup::BackupCliError),
    Secret(keeppeek::mcp::secret_cli::SecretCliError),
}

impl MainError {
    const fn exit_code(&self) -> u8 {
        match self {
            Self::Server(_) => 1,
            Self::Backup(error) => error.exit_code(),
            Self::Secret(error) => error.exit_code(),
        }
    }
}

impl std::fmt::Display for MainError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Server(error) => error.fmt(formatter),
            Self::Backup(error) => error.fmt(formatter),
            Self::Secret(error) => error.fmt(formatter),
        }
    }
}

fn run() -> Result<RunOutcome, MainError> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Config(args)) => {
            return keeppeek_backup::run(args)
                .map(|()| RunOutcome::Finished)
                .map_err(MainError::Backup);
        }
        Some(Command::Mcp(args)) => {
            return keeppeek::mcp::stdio::run(args)
                .map(|()| RunOutcome::Finished)
                .map_err(MainError::Server);
        }
        Some(Command::Secret(args)) => {
            return keeppeek::mcp::secret_cli::run(args)
                .map(|()| RunOutcome::Finished)
                .map_err(MainError::Secret);
        }
        None => {}
    }
    let _ = cli.config;
    run_server(cli.tui).map_err(MainError::Server)
}

fn run_server(tui: keeppeek::tui::DashboardMode) -> anyhow::Result<RunOutcome> {
    let (cfg, config_path) = config::load()?;
    let logging = initialize_global_logging(&config_path)?;

    info!("Starting KeepPeek - press Ctrl+C to stop");

    let shutdown = Shutdown::new();
    let restart = Restart::default();
    let signal_shutdown = shutdown.clone();
    #[cfg(windows)]
    enable_windows_ctrl_c();
    ctrlc::set_handler(move || {
        tracing::info!("shutting down");
        signal_shutdown.cancel();
    })
    .map_err(|error| anyhow::anyhow!("unable to install shutdown signal handler: {error}"))?;

    if keeppeek::app::run_with_dashboard(cfg, &config_path, logging, shutdown, restart, tui)? {
        if keeppeek_supervisor::is_supervised_process() {
            tracing::info!(
                exit_code = keeppeek_supervisor::EXIT_CODE_RESTART_REQUESTED,
                "exiting so parent supervisor can cleanly restart or upgrade KeepPeek"
            );
            return Ok(RunOutcome::SupervisedRestart);
        }
        tracing::info!("restarting to apply camera configuration");
        restart_current_process()?;
    }

    Ok(RunOutcome::Finished)
}

#[cfg(windows)]
fn enable_windows_ctrl_c() {
    // Processes spawned under CREATE_NEW_PROCESS_GROUP or a parent that set IGNORE_CTRL_C
    // inherit ignored Ctrl+C delivery even after registering a console handler.
    // SAFETY: Passing `None` with `false` clears only the calling process's inherited
    // `IGNORE_CTRL_C` attribute; no callback pointer is registered or dereferenced.
    if let Err(error) =
        unsafe { windows::Win32::System::Console::SetConsoleCtrlHandler(None, false) }
    {
        tracing::warn!(%error, "unable to clear inherited Ctrl+C ignore attribute");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_cli_accepts_export_and_apply() {
        assert!(
            Cli::try_parse_from([
                "keeppeek",
                "config",
                "export",
                "--output",
                "configuration.zip",
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "keeppeek",
                "config",
                "apply",
                "configuration.zip",
                "--confirm",
            ])
            .is_ok()
        );
    }

    #[test]
    fn config_cli_requires_export_destination_and_retires_managed_commands() {
        assert!(Cli::try_parse_from(["keeppeek", "config", "export"]).is_err());
        assert!(Cli::try_parse_from(["keeppeek", "backup", "list"]).is_err());
        assert!(Cli::try_parse_from(["keeppeek", "config", "create"]).is_err());
    }

    #[test]
    fn mcp_cli_accepts_bridge_flags_and_skill_subcommand() {
        assert!(
            Cli::try_parse_from([
                "keeppeek",
                "mcp",
                "--server",
                "https://keeppeek.example.com",
                "--credential-file",
                "token.txt",
                "--read-only",
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "keeppeek",
                "mcp",
                "skill",
                "--output",
                "skills/keeppeek-mcp"
            ])
            .is_ok()
        );
        assert!(Cli::try_parse_from(["keeppeek", "mcp", "skill", "--list"]).is_ok());
        assert!(
            Cli::try_parse_from([
                "keeppeek",
                "mcp",
                "skill",
                "camera-debug",
                "--output",
                "skills/keeppeek-camera-debug"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from(["keeppeek", "mcp", "skill", "--list", "--output", "out"]).is_err()
        );
    }
}
