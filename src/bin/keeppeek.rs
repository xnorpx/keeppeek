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
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Export or apply configuration and secrets through the HTTP API.
    Config(keeppeek_backup::BackupArgs),
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::from(error.exit_code())
        }
    }
}

enum MainError {
    Server(anyhow::Error),
    Backup(keeppeek_backup::BackupCliError),
}

impl MainError {
    const fn exit_code(&self) -> u8 {
        match self {
            Self::Server(_) => 1,
            Self::Backup(error) => error.exit_code(),
        }
    }
}

impl std::fmt::Display for MainError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Server(error) => error.fmt(formatter),
            Self::Backup(error) => error.fmt(formatter),
        }
    }
}

fn run() -> Result<(), MainError> {
    let cli = Cli::parse();
    if let Some(Command::Config(args)) = cli.command {
        return keeppeek_backup::run(args).map_err(MainError::Backup);
    }
    let _ = cli.config;
    run_server().map_err(MainError::Server)
}

fn run_server() -> anyhow::Result<()> {
    let (cfg, config_path) = config::load()?;
    let logging = initialize_global_logging(&config_path)?;

    info!("Starting KeepPeek - press Ctrl+C to stop");

    let shutdown = Shutdown::new();
    let restart = Restart::default();
    let signal_shutdown = shutdown.clone();
    ctrlc::set_handler(move || {
        tracing::info!("shutting down");
        signal_shutdown.cancel();
    })
    .map_err(|error| anyhow::anyhow!("unable to install shutdown signal handler: {error}"))?;

    if keeppeek::app::run(cfg, &config_path, logging, shutdown, restart)? {
        tracing::info!("restarting to apply camera configuration");
        restart_current_process()?;
    }

    Ok(())
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
}
