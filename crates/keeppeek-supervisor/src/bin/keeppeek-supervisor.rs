//! Standalone cross-platform CLI host for the KeepPeek slim supervisor.
//!
//! Runs as a foreground supervisor (or under `systemd` / `launchd`) and provides
//! offline subcommands to stage Ed25519-signed binaries, inspect A/B slot state,
//! and roll back to a previous release.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::{Args, Parser, Subcommand, ValueEnum};
use keeppeek_supervisor::manifest::{
    ComponentKind, TrustedSigningKey, current_platform, decode_public_key_hex, read_manifest_file,
};
use keeppeek_supervisor::slots::{DEFAULT_MAX_BOOT_ATTEMPTS, SlotStore, StageOptions};
use keeppeek_supervisor::supervisor::{
    ComponentConfig, ComponentPhase, DEFAULT_PROBATION_MS, DEFAULT_READINESS_TIMEOUT_MS,
    DEFAULT_STOP_GRACE_MS, SupervisorConfig, SupervisorHandle,
};
use tracing_subscriber::EnvFilter;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Debug, Parser)]
#[command(name = "keeppeek-supervisor", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ComponentArg {
    Core,
    Vision,
}

impl From<ComponentArg> for ComponentKind {
    fn from(value: ComponentArg) -> Self {
        match value {
            ComponentArg::Core => Self::Core,
            ComponentArg::Vision => Self::Vision,
        }
    }
}

#[derive(Debug, Args)]
struct RunArgs {
    /// Supervisor root directory holding `supervisor-state.json` and version slots.
    #[arg(long, value_name = "DIR")]
    root: PathBuf,
    /// Optional path to `config.toml` forwarded to `keeppeek --config`.
    #[arg(short = 'c', long, value_name = "PATH")]
    config: Option<PathBuf>,
    /// Fallback `keeppeek` executable used before any A/B slot is staged.
    #[arg(long, value_name = "PATH")]
    fallback_bin: Option<PathBuf>,
    /// Optional fallback `keeppeek-vision` executable to supervise independently.
    #[arg(long, value_name = "PATH")]
    vision_fallback_bin: Option<PathBuf>,
    /// Enable independent Vision slot supervision even without a fallback binary.
    #[arg(long)]
    enable_vision: bool,
    /// Readiness timeout in milliseconds.
    #[arg(long, default_value_t = DEFAULT_READINESS_TIMEOUT_MS)]
    readiness_timeout_ms: u64,
    /// Pending upgrade probation window in milliseconds.
    #[arg(long, default_value_t = DEFAULT_PROBATION_MS)]
    probation_ms: u64,
    /// Graceful child stop grace period in milliseconds.
    #[arg(long, default_value_t = DEFAULT_STOP_GRACE_MS)]
    stop_grace_ms: u64,
}

#[derive(Debug, Args)]
struct StageArgs {
    /// Supervisor root directory.
    #[arg(long, value_name = "DIR")]
    root: PathBuf,
    /// Path to the signed release manifest JSON file.
    #[arg(long, value_name = "PATH")]
    manifest: PathBuf,
    /// Path to the executable binary covered by the manifest.
    #[arg(long, value_name = "PATH")]
    binary: PathBuf,
    /// Trusted signing key identifier matching `payload.signing_key_id`.
    #[arg(long, value_name = "ID")]
    key_id: String,
    /// Trusted 64-character hex Ed25519 public key.
    #[arg(long, value_name = "HEX")]
    public_key_hex: String,
    /// Allow staging a manifest marked with breaking schema compatibility.
    #[arg(long)]
    allow_breaking: bool,
    /// Maximum boot attempts before automatic rollback.
    #[arg(long, default_value_t = DEFAULT_MAX_BOOT_ATTEMPTS)]
    max_boot_attempts: u32,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Supervise KeepPeek (and optionally KeepPeek Vision) child executables.
    Run(RunArgs),
    /// Verify an Ed25519-signed release manifest and stage its binary into an immutable A/B slot.
    Stage(StageArgs),
    /// Display persisted A/B slot state for core and Vision components.
    Status {
        /// Supervisor root directory.
        #[arg(long, value_name = "DIR")]
        root: PathBuf,
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Roll a component's active slot back to its recorded previous version.
    Rollback {
        /// Supervisor root directory.
        #[arg(long, value_name = "DIR")]
        root: PathBuf,
        /// Component to roll back.
        #[arg(long, value_enum, default_value_t = ComponentArg::Core)]
        component: ComponentArg,
    },
}

fn main() -> ExitCode {
    init_logging();
    let cli = Cli::parse();
    match execute(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("keeppeek-supervisor error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn execute(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::Run(args) => run_supervisor(args),
        Command::Stage(args) => run_stage(&args),
        Command::Status { root, json } => run_status(&root, json),
        Command::Rollback { root, component } => run_rollback(&root, component.into()),
    }
}

fn run_supervisor(args: RunArgs) -> Result<(), String> {
    let mut supervisor_config = SupervisorConfig::new(args.root);
    supervisor_config.core.fallback_binary = args.fallback_bin.or_else(default_sibling_core_binary);
    supervisor_config.core.readiness_timeout_ms = args.readiness_timeout_ms;
    supervisor_config.core.probation_ms = args.probation_ms;
    supervisor_config.core.stop_grace_ms = args.stop_grace_ms;
    if let Some(path) = args.config {
        supervisor_config.core.args.push(OsString::from("--config"));
        supervisor_config.core.args.push(path.into_os_string());
    }
    supervisor_config.core.args.push(OsString::from("--tui"));
    supervisor_config.core.args.push(OsString::from("off"));

    if args.enable_vision || args.vision_fallback_bin.is_some() {
        let mut vision = ComponentConfig::new(ComponentKind::Vision);
        vision.fallback_binary = args.vision_fallback_bin;
        vision.readiness_timeout_ms = args.readiness_timeout_ms;
        vision.probation_ms = args.probation_ms;
        vision.stop_grace_ms = args.stop_grace_ms;
        supervisor_config.vision = Some(vision);
    }

    let stop_flag = Arc::new(AtomicBool::new(false));
    let handler_flag = Arc::clone(&stop_flag);
    ctrlc::set_handler(move || {
        handler_flag.store(true, Ordering::Release);
    })
    .map_err(|error| format!("unable to install termination signal handler: {error}"))?;

    let mut handle = SupervisorHandle::start(supervisor_config).map_err(|e| e.to_string())?;
    while !stop_flag.load(Ordering::Acquire) {
        let snap = handle.snapshot();
        if matches!(
            snap.core.phase,
            ComponentPhase::Stopped | ComponentPhase::Failed
        ) && snap.core.last_error.is_some()
        {
            handle.stop();
            return Err(snap
                .core
                .last_error
                .unwrap_or_else(|| "core component failed".to_owned()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    handle.stop();
    Ok(())
}

fn run_stage(args: &StageArgs) -> Result<(), String> {
    let public_key_bytes =
        decode_public_key_hex(&args.public_key_hex).map_err(|e| e.to_string())?;
    let manifest = read_manifest_file(&args.manifest).map_err(|e| e.to_string())?;
    let now_unix_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("system clock error: {e}"))?
        .as_secs();
    let store = SlotStore::new(&args.root);
    let state = store
        .stage_signed_binary(
            &manifest,
            &args.binary,
            &[TrustedSigningKey {
                key_id: args.key_id.clone(),
                public_key_bytes,
            }],
            &StageOptions {
                expected_platform: current_platform(),
                now_unix_secs,
                allow_breaking: args.allow_breaking,
                max_boot_attempts: args.max_boot_attempts,
            },
        )
        .map_err(|e| e.to_string())?;
    let rendered = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
    println!("{rendered}");
    Ok(())
}

fn run_status(root: &Path, json: bool) -> Result<(), String> {
    let store = SlotStore::new(root);
    let state = store.load_state().map_err(|e| e.to_string())?;
    if json {
        let rendered = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
        println!("{rendered}");
        return Ok(());
    }
    println!("Supervisor root: {}", store.root().display());
    print_slot_summary("core", &state.core);
    print_slot_summary("vision", &state.vision);
    Ok(())
}

fn run_rollback(root: &Path, component: ComponentKind) -> Result<(), String> {
    let store = SlotStore::new(root);
    let state = store
        .manual_rollback(component)
        .map_err(|e| e.to_string())?;
    let slot = state.component(component);
    println!(
        "Rolled back `{component}` to active={:?} (previous={:?})",
        slot.active, slot.previous
    );
    Ok(())
}

fn print_slot_summary(label: &str, slot: &keeppeek_supervisor::slots::ComponentSlotState) {
    println!(
        "  {label}: active={} previous={} pending={} floor={} quarantined={}",
        slot.active
            .map_or_else(|| "none".to_owned(), |v| v.to_string()),
        slot.previous
            .map_or_else(|| "none".to_owned(), |v| v.to_string()),
        slot.pending
            .as_ref()
            .map_or_else(|| "none".to_owned(), |p| p.version.to_string()),
        slot.min_version_floor
            .map_or_else(|| "none".to_owned(), |v| v.to_string()),
        slot.quarantined.len(),
    );
}

fn default_sibling_core_binary() -> Option<PathBuf> {
    let sibling = std::env::current_exe()
        .ok()?
        .parent()?
        .join(ComponentKind::Core.current_binary_file_name());
    sibling.is_file().then_some(sibling)
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_all_supervisor_subcommands() {
        assert!(Cli::try_parse_from(["keeppeek-supervisor", "run", "--root", "sup"]).is_ok());
        assert!(
            Cli::try_parse_from(["keeppeek-supervisor", "status", "--root", "sup", "--json"])
                .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "keeppeek-supervisor",
                "rollback",
                "--root",
                "sup",
                "--component",
                "vision"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "keeppeek-supervisor",
                "stage",
                "--root",
                "sup",
                "--manifest",
                "manifest.json",
                "--binary",
                "keeppeek",
                "--key-id",
                "release-2026",
                "--public-key-hex",
                &"ab".repeat(32),
                "--allow-breaking",
            ])
            .is_ok()
        );
    }
}
