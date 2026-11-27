#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

#[cfg(any(target_os = "windows", target_os = "macos"))]
mod desktop_app;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use keeppeek_desktop::config_store::{self, SERVER_URL_ENV};
use tracing_subscriber::EnvFilter;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Debug, Parser)]
#[command(name = "keeppeek-desktop", version)]
struct Cli {
    /// Use a specific KeepPeek configuration file.
    #[arg(short = 'c', long, value_name = "PATH")]
    config: Option<PathBuf>,
    /// Connect to a specific KeepPeek server URL instead of the configured default.
    #[arg(long, value_name = "URL")]
    server_url: Option<String>,
    /// Opt in to supervising a local `keeppeek` child process from KeepPeek Desktop.
    #[arg(long)]
    manage_local_server: bool,
    /// Override the supervisor root directory used in managed local server mode.
    #[arg(long, value_name = "DIR")]
    supervisor_root: Option<PathBuf>,
    /// Override the fallback `keeppeek` executable path used in managed local server mode.
    #[arg(long, value_name = "PATH")]
    server_bin: Option<PathBuf>,
    /// Open the connection launcher instead of connecting automatically on startup.
    #[arg(long)]
    no_autoconnect: bool,
    /// Write startup timing and connection diagnostics as JSON to this path.
    #[arg(long, value_name = "PATH", hide = true)]
    diagnostics_file: Option<PathBuf>,
    /// Exit cleanly this many milliseconds after the server loads or the startup probe fails.
    #[arg(long, value_name = "MS", hide = true)]
    smoke_exit_ms: Option<u64>,
}

fn main() -> ExitCode {
    init_logging();
    let cli = Cli::parse();
    let config_path = cli.config.unwrap_or_else(config_store::default_config_path);
    let env_override = std::env::var(SERVER_URL_ENV).ok();
    let target = match config_store::resolve_target_with_options(
        &config_path,
        cli.server_url.as_deref(),
        env_override.as_deref(),
        cli.manage_local_server,
        cli.supervisor_root.as_deref(),
    ) {
        Ok(target) => target,
        Err(error) => {
            eprintln!("KeepPeek Desktop configuration error: {error}");
            return ExitCode::FAILURE;
        }
    };

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        let options = desktop_app::LaunchOptions {
            target,
            auto_connect: !cli.no_autoconnect,
            server_bin: cli.server_bin,
            diagnostics_file: cli.diagnostics_file,
            smoke_exit_ms: cli.smoke_exit_ms,
        };
        if let Err(error) = desktop_app::run(options) {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
        ExitCode::SUCCESS
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = (
            cli.no_autoconnect,
            cli.server_bin,
            cli.diagnostics_file,
            cli.smoke_exit_ms,
        );
        eprintln!(
            "KeepPeek Desktop targets Windows and macOS only. Open {} in a browser on this platform.",
            target.origin
        );
        ExitCode::FAILURE
    }
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}
