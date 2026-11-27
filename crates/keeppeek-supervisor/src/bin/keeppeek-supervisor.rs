//! Standalone cross-platform CLI host for the KeepPeek slim supervisor.
//!
//! Runs as a foreground supervisor (or under `systemd` / `launchd`), checks and
//! downloads `release` or `main` (`master`) builds from GitHub Releases, stages
//! local developer builds, signs Manifest v2 artifacts, and triggers live
//! zero-friction apply/wait swaps across CLI, Windows service, and Desktop hosts.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::{Args, Parser, Subcommand, ValueEnum};
use keeppeek_supervisor::control::{
    ControlAction, LiveSupervisorStatus, is_supervisor_running, read_live_status,
    wait_for_control_completion, write_control_request,
};
use keeppeek_supervisor::fetch::{
    ComponentReleaseSource, RemoteUpdateOptions, check_remote_channel, fetch_and_stage_remote,
};
use keeppeek_supervisor::manifest::{
    ComponentKind, DEFAULT_STORAGE_SCHEMA, MANIFEST_SCHEMA_VERSION_V2, ReleaseManifestPayload,
    SchemaCompatibility, TrustedSigningKey, current_platform, decode_public_key_hex,
    decode_seed_hex, hash_binary_file, pinned_keys_for_channel, read_manifest_file, sign_manifest,
};
use keeppeek_supervisor::slots::{
    DEFAULT_MAX_BOOT_ATTEMPTS, LaunchPolicy, SlotStore, StageOptions, SupervisorState,
};
use keeppeek_supervisor::supervisor::{
    ComponentConfig, ComponentPhase, DEFAULT_PROBATION_MS, DEFAULT_READINESS_TIMEOUT_MS,
    DEFAULT_STOP_GRACE_MS, SupervisorConfig, SupervisorHandle,
};
use keeppeek_supervisor::version::{ReleaseVersion, UpdateChannel, current_supervisor_version};
use serde::Serialize;
use tracing_subscriber::EnvFilter;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const DEFAULT_APPLY_WAIT_TIMEOUT_MS: u64 = 45_000;
const SIGNING_SEED_ENV: &str = "KEEPPEEK_RELEASE_SIGNING_SEED_HEX";

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

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ChannelArg {
    Release,
    Main,
    Local,
}

impl From<ChannelArg> for UpdateChannel {
    fn from(value: ChannelArg) -> Self {
        match value {
            ChannelArg::Release => Self::Release,
            ChannelArg::Main => Self::Main,
            ChannelArg::Local => Self::Local,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RemoteChannelArg {
    Release,
    Main,
}

impl From<RemoteChannelArg> for UpdateChannel {
    fn from(value: RemoteChannelArg) -> Self {
        match value {
            RemoteChannelArg::Release => Self::Release,
            RemoteChannelArg::Main => Self::Main,
        }
    }
}

#[derive(Debug, Args)]
struct RunArgs {
    /// Supervisor root directory holding `supervisor-state.json`, version slots, and `runtime/`.
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
    /// Explicitly opt in to launching unsigned `local-*` developer slots (enforces root ACL check).
    #[arg(long)]
    allow_local: bool,
    /// Explicitly opt in to launching unsigned SHA-256-verified `main` slots.
    #[arg(long)]
    allow_unsigned_main: bool,
    /// Optional custom Ed25519 key id trusted ONLY for `release` slots.
    #[arg(long, value_name = "ID")]
    release_key_id: Option<String>,
    /// Optional 64-hex Ed25519 public key paired with `--release-key-id`.
    #[arg(long, value_name = "HEX")]
    release_public_key_hex: Option<String>,
    /// Optional custom Ed25519 key id trusted ONLY for `main` slots.
    #[arg(long, value_name = "ID")]
    main_key_id: Option<String>,
    /// Optional 64-hex Ed25519 public key paired with `--main-key-id`.
    #[arg(long, value_name = "HEX")]
    main_public_key_hex: Option<String>,
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
    /// Path to the signed Manifest v2 JSON file.
    #[arg(long, value_name = "PATH")]
    manifest: PathBuf,
    /// Path to the executable binary covered by the manifest.
    #[arg(long, value_name = "PATH")]
    binary: PathBuf,
    /// Expected update channel (`release` or `main`).
    #[arg(long, value_enum, default_value_t = RemoteChannelArg::Release)]
    channel: RemoteChannelArg,
    /// Optional explicit signing key identifier override (defaults to pinned channel keys).
    #[arg(long, value_name = "ID")]
    key_id: Option<String>,
    /// Optional 64-character hex Ed25519 public key paired with `--key-id`.
    #[arg(long, value_name = "HEX")]
    public_key_hex: Option<String>,
    /// Allow staging a manifest marked with breaking or higher storage schema compatibility.
    #[arg(long)]
    allow_breaking: bool,
    /// Maximum boot attempts before automatic rollback.
    #[arg(long, default_value_t = DEFAULT_MAX_BOOT_ATTEMPTS)]
    max_boot_attempts: u32,
    /// Signal a running supervisor to apply the staged slot immediately.
    #[arg(long)]
    apply: bool,
    /// Wait for the running supervisor to finish applying and pass probation.
    #[arg(long)]
    wait: bool,
    /// Wait timeout in milliseconds when `--wait` is set.
    #[arg(long, default_value_t = DEFAULT_APPLY_WAIT_TIMEOUT_MS)]
    timeout_ms: u64,
    /// Emit machine-readable JSON output.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct StageLocalArgs {
    /// Supervisor root directory.
    #[arg(long, value_name = "DIR")]
    root: PathBuf,
    /// Path to the locally built executable (for example `target/debug/keeppeek.exe`).
    #[arg(long, value_name = "PATH")]
    binary: PathBuf,
    /// Target component (`core` or `vision`).
    #[arg(long, value_enum, default_value_t = ComponentArg::Core)]
    component: ComponentArg,
    /// Optional package semver label (defaults to compiled supervisor version).
    #[arg(long, value_name = "X.Y.Z")]
    version: Option<String>,
    /// Maximum boot attempts before automatic rollback.
    #[arg(long, default_value_t = 1)]
    max_boot_attempts: u32,
    /// Signal a running supervisor to apply the staged local build immediately.
    #[arg(long)]
    apply: bool,
    /// Wait for the running supervisor to complete cutover and probation.
    #[arg(long)]
    wait: bool,
    /// Wait timeout in milliseconds when `--wait` is set.
    #[arg(long, default_value_t = DEFAULT_APPLY_WAIT_TIMEOUT_MS)]
    timeout_ms: u64,
    /// Emit machine-readable JSON output.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct CheckArgs {
    /// Supervisor root directory.
    #[arg(long, value_name = "DIR")]
    root: PathBuf,
    /// Remote release channel to inspect (`release` or `main`).
    #[arg(long, value_enum, default_value_t = RemoteChannelArg::Release)]
    channel: RemoteChannelArg,
    /// Target component (`core` or `vision`).
    #[arg(long, value_enum, default_value_t = ComponentArg::Core)]
    component: ComponentArg,
    /// GitHub `owner/repo` override (defaults to `xnorpx/keeppeek` for core; required for vision).
    #[arg(long, value_name = "OWNER/REPO")]
    repo: Option<String>,
    /// Print machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct UpdateArgs {
    /// Supervisor root directory.
    #[arg(long, value_name = "DIR")]
    root: PathBuf,
    /// Update channel (`release`, `main`, or `local`).
    #[arg(long, value_enum)]
    channel: ChannelArg,
    /// Target component (`core` or `vision`).
    #[arg(long, value_enum, default_value_t = ComponentArg::Core)]
    component: ComponentArg,
    /// Local binary path (required when `--channel local`).
    #[arg(long, value_name = "PATH")]
    binary: Option<PathBuf>,
    /// Optional package semver label when `--channel local`.
    #[arg(long, value_name = "X.Y.Z")]
    version: Option<String>,
    /// GitHub `owner/repo` override (defaults to `xnorpx/keeppeek` for core; required for vision).
    #[arg(long, value_name = "OWNER/REPO")]
    repo: Option<String>,
    /// Optional explicit signing key identifier override (testing only).
    #[arg(long, value_name = "ID")]
    key_id: Option<String>,
    /// Optional 64-character hex Ed25519 public key paired with `--key-id`.
    #[arg(long, value_name = "HEX")]
    public_key_hex: Option<String>,
    /// Explicit CLI opt-in for unsigned SHA-256 verification on `main` (`master`) builds only.
    #[arg(long)]
    allow_unsigned_sha256: bool,
    /// Allow staging manifests declaring breaking or higher storage schemas.
    #[arg(long)]
    allow_breaking: bool,
    /// Maximum boot attempts before automatic rollback.
    #[arg(long, default_value_t = DEFAULT_MAX_BOOT_ATTEMPTS)]
    max_boot_attempts: u32,
    /// Signal a running supervisor to apply the staged update immediately.
    #[arg(long)]
    apply: bool,
    /// Wait for the running supervisor to complete cutover and probation.
    #[arg(long)]
    wait: bool,
    /// Wait timeout in milliseconds when `--wait` is set.
    #[arg(long, default_value_t = DEFAULT_APPLY_WAIT_TIMEOUT_MS)]
    timeout_ms: u64,
    /// Emit machine-readable JSON output.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ApplyArgs {
    /// Supervisor root directory.
    #[arg(long, value_name = "DIR")]
    root: PathBuf,
    /// Target component (`core` or `vision`).
    #[arg(long, value_enum, default_value_t = ComponentArg::Core)]
    component: ComponentArg,
    /// Force a clean restart even when no `pending` slot is staged.
    #[arg(long)]
    restart_if_no_pending: bool,
    /// Block until the running supervisor finishes the control request.
    #[arg(long)]
    wait: bool,
    /// Wait timeout in milliseconds when `--wait` is set.
    #[arg(long, default_value_t = DEFAULT_APPLY_WAIT_TIMEOUT_MS)]
    timeout_ms: u64,
    /// Emit machine-readable JSON output.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct SignArgs {
    /// Release channel encoded into Manifest v2 (`release` or `main`).
    #[arg(long, value_enum)]
    channel: RemoteChannelArg,
    /// Target component (`core` or `vision`).
    #[arg(long, value_enum, default_value_t = ComponentArg::Core)]
    component: ComponentArg,
    /// Canonical `major.minor.patch` version string.
    #[arg(long, value_name = "X.Y.Z")]
    version: String,
    /// Monotonic CI build sequence (`0` for `release`, positive `run_number` for `main`).
    #[arg(long, default_value_t = 0)]
    build_seq: u64,
    /// Git commit SHA (7..=40 lowercase hex characters).
    #[arg(long, value_name = "HEX")]
    commit_sha: String,
    /// Storage schema revision encoded into the manifest.
    #[arg(long, default_value_t = DEFAULT_STORAGE_SCHEMA)]
    storage_schema: u32,
    /// Target `<os>-<arch>` platform string (defaults to current host).
    #[arg(long, value_name = "OS-ARCH")]
    platform: Option<String>,
    /// Path to the built executable binary to hash and sign.
    #[arg(long, value_name = "PATH")]
    binary: PathBuf,
    /// Signing key identifier matching the verifier's trusted key ID.
    #[arg(long, value_name = "ID")]
    key_id: String,
    /// Path to a private file containing the 64-hex Ed25519 seed (falls back to `KEEPPEEK_RELEASE_SIGNING_SEED_HEX`).
    #[arg(long, value_name = "PATH")]
    seed_file: Option<PathBuf>,
    /// Output path for the signed Manifest v2 JSON file.
    #[arg(long, value_name = "PATH")]
    output: PathBuf,
    /// Mark schema compatibility as `breaking` instead of `expand_contract`.
    #[arg(long)]
    breaking: bool,
    /// Validity window in days (1..=365).
    #[arg(long, default_value_t = 90)]
    valid_days: u64,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Supervise KeepPeek (and optionally KeepPeek Vision) child executables.
    Run(RunArgs),
    /// Verify an Ed25519-signed Manifest v2 and stage its binary into an immutable slot.
    Stage(StageArgs),
    /// Stage an unsigned local developer build into a content-addressed `local-*` slot.
    StageLocal(StageLocalArgs),
    /// Check GitHub Releases for compatible `release` or `main` updates and newer major versions.
    Check(CheckArgs),
    /// Fetch/stage an update from `release`, `main`, or `local`, with optional live `--apply --wait`.
    Update(UpdateArgs),
    /// Signal a running supervisor (CLI, service, or Desktop) to apply pending slots or restart.
    Apply(ApplyArgs),
    /// Display persisted A/B slot state and live supervisor runtime status.
    Status {
        /// Supervisor root directory.
        #[arg(long, value_name = "DIR")]
        root: PathBuf,
        /// Print machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Roll a component's active slot back to its recorded previous slot.
    Rollback(RollbackArgs),
    /// Generate and sign a Manifest v2 JSON file for a release or `main` binary.
    Sign(SignArgs),
}

#[derive(Debug, Args)]
struct RollbackArgs {
    /// Supervisor root directory.
    #[arg(long, value_name = "DIR")]
    root: PathBuf,
    /// Component to roll back.
    #[arg(long, value_enum, default_value_t = ComponentArg::Core)]
    component: ComponentArg,
    /// Allow rolling back to a `local` slot when rolling back offline.
    #[arg(long)]
    allow_local: bool,
    /// Allow rolling back to an unsigned SHA-256 `main` slot.
    #[arg(long)]
    allow_unsigned_main: bool,
    /// Optional custom Ed25519 key id trusted ONLY for `release` slots.
    #[arg(long, value_name = "ID")]
    release_key_id: Option<String>,
    /// Optional 64-hex Ed25519 public key paired with `--release-key-id`.
    #[arg(long, value_name = "HEX")]
    release_public_key_hex: Option<String>,
    /// Optional custom Ed25519 key id trusted ONLY for `main` slots.
    #[arg(long, value_name = "ID")]
    main_key_id: Option<String>,
    /// Optional 64-hex Ed25519 public key paired with `--main-key-id`.
    #[arg(long, value_name = "HEX")]
    main_public_key_hex: Option<String>,
    /// Signal a running supervisor to restart onto the rolled-back active slot.
    #[arg(long)]
    apply: bool,
    /// Wait for the running supervisor to restart cleanly.
    #[arg(long)]
    wait: bool,
    /// Wait timeout in milliseconds when `--wait` is set.
    #[arg(long, default_value_t = DEFAULT_APPLY_WAIT_TIMEOUT_MS)]
    timeout_ms: u64,
    /// Emit machine-readable JSON output.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Serialize)]
struct CombinedStatusOutput {
    root: String,
    supervisor_running: bool,
    persisted: SupervisorState,
    #[serde(skip_serializing_if = "Option::is_none")]
    live: Option<LiveSupervisorStatus>,
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
        Command::StageLocal(args) => run_stage_local(&args),
        Command::Check(args) => run_check(&args),
        Command::Update(args) => run_update(&args),
        Command::Apply(args) => run_apply(&args),
        Command::Status { root, json } => run_status(&root, json),
        Command::Rollback(args) => run_rollback(&args),
        Command::Sign(args) => run_sign(&args),
    }
}

fn run_supervisor(args: RunArgs) -> Result<(), String> {
    let mut supervisor_config = SupervisorConfig::new(args.root);
    supervisor_config.core.fallback_binary = args.fallback_bin.or_else(default_sibling_core_binary);
    supervisor_config.core.readiness_timeout_ms = args.readiness_timeout_ms;
    supervisor_config.core.probation_ms = args.probation_ms;
    supervisor_config.core.stop_grace_ms = args.stop_grace_ms;
    supervisor_config.launch_policy.allow_local = args.allow_local;
    supervisor_config.launch_policy.allow_unsigned_main = args.allow_unsigned_main;
    supervisor_config.enforce_root_acl = true;
    if let Some(release_key) = parse_optional_key(
        args.release_key_id.as_deref(),
        args.release_public_key_hex.as_deref(),
    )? {
        supervisor_config
            .launch_policy
            .release_keys
            .push(release_key);
    }
    if let Some(main_key) = parse_optional_key(
        args.main_key_id.as_deref(),
        args.main_public_key_hex.as_deref(),
    )? {
        supervisor_config.launch_policy.main_keys.push(main_key);
    }

    if let Some(path) = args.config {
        supervisor_config.core.args.push(OsString::from("--config"));
        supervisor_config.core.args.push(path.into_os_string());
    }
    supervisor_config.core.args.push(OsString::from("--tui"));
    supervisor_config.core.args.push(OsString::from("never"));

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

    let allow_local = args.allow_local;
    let mut handle = SupervisorHandle::start(supervisor_config).map_err(|e| e.to_string())?;
    while !stop_flag.load(Ordering::Acquire) {
        let snap = handle.snapshot();
        if !allow_local
            && matches!(
                snap.core.phase,
                ComponentPhase::Stopped | ComponentPhase::Failed
            )
            && snap.core.last_error.is_some()
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
    let channel: UpdateChannel = args.channel.into();
    let trusted_keys = resolve_trusted_keys(
        channel,
        args.key_id.as_deref(),
        args.public_key_hex.as_deref(),
    )?;
    let manifest = read_manifest_file(&args.manifest).map_err(|e| e.to_string())?;
    let now_unix_secs = now_unix_secs()?;
    let store = SlotStore::new(&args.root);
    let outcome = store
        .stage_signed_binary(
            &manifest,
            &args.binary,
            &trusted_keys,
            &StageOptions {
                expected_channel: channel,
                expected_platform: current_platform(),
                supervisor_version: current_supervisor_version(),
                now_unix_secs,
                allow_breaking: args.allow_breaking,
                max_boot_attempts: args.max_boot_attempts,
            },
        )
        .map_err(|e| e.to_string())?;
    finish_stage_and_maybe_apply(
        &args.root,
        manifest.payload.component,
        outcome.slot_id.as_str(),
        outcome.changed,
        ApplyWaitOptions {
            apply: args.apply,
            wait: args.wait,
            timeout_ms: args.timeout_ms,
            json: args.json,
        },
    )
}

fn run_stage_local(args: &StageLocalArgs) -> Result<(), String> {
    let component: ComponentKind = args.component.into();
    let version = parse_optional_version(args.version.as_deref())?;
    let now_unix_secs = now_unix_secs()?;
    let store = SlotStore::new(&args.root);
    let outcome = store
        .stage_local_binary(
            component,
            &args.binary,
            version,
            now_unix_secs,
            args.max_boot_attempts,
        )
        .map_err(|e| e.to_string())?;
    finish_stage_and_maybe_apply(
        &args.root,
        component,
        outcome.slot_id.as_str(),
        outcome.changed,
        ApplyWaitOptions {
            apply: args.apply,
            wait: args.wait,
            timeout_ms: args.timeout_ms,
            json: args.json,
        },
    )
}

fn run_check(args: &CheckArgs) -> Result<(), String> {
    let component: ComponentKind = args.component.into();
    let channel: UpdateChannel = args.channel.into();
    let source = resolve_component_source(component, args.repo.as_deref())?;
    let store = SlotStore::new(&args.root);
    let report = check_remote_channel(
        &store,
        component,
        channel,
        &source,
        &current_platform(),
        current_supervisor_version(),
        now_unix_secs()?,
    )
    .map_err(|e| e.to_string())?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    println!(
        "Channel `{}` for `{}` (supervisor {}):",
        report.channel, report.component, report.supervisor_version
    );
    if report.not_modified {
        println!(
            "  status: remote feed unchanged since last check (HTTP 304 Not Modified); run `update` to stage"
        );
    } else {
        println!(
            "  compatible_tag: {}",
            report.compatible_tag.as_deref().unwrap_or("none")
        );
        println!("  signed_manifest: {}", report.signed_manifest_available);
        println!(
            "  standalone_binary: {}",
            report.standalone_binary_available
        );
        if let Some(newer) = report.newer_major_available {
            println!(
                "  newer_major_available: {newer} (requires fresh install to upgrade supervisor)"
            );
        }
    }
    Ok(())
}

fn run_update(args: &UpdateArgs) -> Result<(), String> {
    let component: ComponentKind = args.component.into();
    let channel: UpdateChannel = args.channel.into();
    if channel == UpdateChannel::Local {
        let Some(binary) = args.binary.as_ref() else {
            return Err("`update --channel local` requires `--binary <PATH>`".to_owned());
        };
        let local_args = StageLocalArgs {
            root: args.root.clone(),
            binary: binary.clone(),
            component: args.component,
            version: args.version.clone(),
            max_boot_attempts: args.max_boot_attempts.max(1),
            apply: args.apply,
            wait: args.wait,
            timeout_ms: args.timeout_ms,
            json: args.json,
        };
        return run_stage_local(&local_args);
    }

    if channel == UpdateChannel::Release && args.allow_unsigned_sha256 {
        return Err("`--allow-unsigned-sha256` is forbidden on the `release` channel".to_owned());
    }
    let source = resolve_component_source(component, args.repo.as_deref())?;
    let trusted_keys = resolve_trusted_keys(
        channel,
        args.key_id.as_deref(),
        args.public_key_hex.as_deref(),
    )?;
    let store = SlotStore::new(&args.root);
    let outcome = fetch_and_stage_remote(
        &store,
        &RemoteUpdateOptions {
            component,
            channel,
            source,
            platform: current_platform(),
            supervisor_version: current_supervisor_version(),
            trusted_keys,
            allow_unsigned_sha256: args.allow_unsigned_sha256,
            allow_breaking: args.allow_breaking,
            now_unix_secs: now_unix_secs()?,
            max_boot_attempts: args.max_boot_attempts,
        },
    )
    .map_err(|e| e.to_string())?;

    let Some(staged) = outcome.staged else {
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "changed": false,
                    "not_modified": outcome.check.not_modified,
                    "check": outcome.check,
                }))
                .map_err(|e| e.to_string())?
            );
        } else {
            println!("No update downloaded (remote feed unchanged).");
        }
        return Ok(());
    };
    finish_stage_and_maybe_apply(
        &args.root,
        component,
        staged.slot_id.as_str(),
        staged.changed,
        ApplyWaitOptions {
            apply: args.apply,
            wait: args.wait,
            timeout_ms: args.timeout_ms,
            json: args.json,
        },
    )
}

fn run_apply(args: &ApplyArgs) -> Result<(), String> {
    let component: ComponentKind = args.component.into();
    let action = if args.restart_if_no_pending {
        ControlAction::Restart
    } else {
        ControlAction::ApplyPending
    };
    let request =
        write_control_request(&args.root, component, action).map_err(|e| e.to_string())?;
    if !args.wait {
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&request).map_err(|e| e.to_string())?
            );
        } else {
            println!(
                "Sent `{:?}` control request `{}` for `{component}`",
                action, request.request_id
            );
        }
        return Ok(());
    }
    let snapshot =
        wait_for_control_completion(&args.root, component, &request.request_id, args.timeout_ms)
            .map_err(|e| e.to_string())?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&snapshot).map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "Applied `{component}`: pid={:?} slot={:?} restarts={} rollbacks={}",
            snapshot.pid, snapshot.running_slot, snapshot.restart_count, snapshot.rollback_count
        );
    }
    Ok(())
}

fn run_status(root: &Path, json: bool) -> Result<(), String> {
    let store = SlotStore::new(root);
    let persisted = store.load_state().map_err(|e| e.to_string())?;
    let running = is_supervisor_running(root);
    let live = read_live_status(root).map_err(|e| e.to_string())?;
    if json {
        let output = CombinedStatusOutput {
            root: store.root().display().to_string(),
            supervisor_running: running,
            persisted,
            live,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&output).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    println!(
        "Supervisor root: {} (running={running})",
        store.root().display()
    );
    print_slot_summary("core", &persisted.core);
    print_slot_summary("vision", &persisted.vision);
    if let Some(live_status) = live {
        println!(
            "  live: supervisor_pid={} allow_local={} core_phase={:?} core_pid={:?} core_slot={:?}",
            live_status.supervisor_pid,
            live_status.allow_local,
            live_status.core.phase,
            live_status.core.pid,
            live_status.core.running_slot,
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct ApplyWaitOptions {
    apply: bool,
    wait: bool,
    timeout_ms: u64,
    json: bool,
}

fn run_rollback(args: &RollbackArgs) -> Result<(), String> {
    let component: ComponentKind = args.component.into();
    let store = SlotStore::new(&args.root);
    let mut policy = LaunchPolicy {
        allow_local: args.allow_local,
        allow_unsigned_main: args.allow_unsigned_main,
        ..LaunchPolicy::default()
    };
    if let Some(release_key) = parse_optional_key(
        args.release_key_id.as_deref(),
        args.release_public_key_hex.as_deref(),
    )? {
        policy.release_keys.push(release_key);
    }
    if let Some(main_key) = parse_optional_key(
        args.main_key_id.as_deref(),
        args.main_public_key_hex.as_deref(),
    )? {
        policy.main_keys.push(main_key);
    }
    let state = store
        .manual_rollback(component, &policy)
        .map_err(|e| e.to_string())?;
    let active_slot = state
        .component(component)
        .active
        .as_ref()
        .map_or_else(|| "none".to_owned(), ToString::to_string);
    if args.apply || args.wait {
        let request = write_control_request(&args.root, component, ControlAction::Restart)
            .map_err(|e| e.to_string())?;
        if args.wait {
            let snap = wait_for_control_completion(
                &args.root,
                component,
                &request.request_id,
                args.timeout_ms,
            )
            .map_err(|e| e.to_string())?;
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&snap).map_err(|e| e.to_string())?
                );
            } else {
                println!(
                    "Rolled back and restarted `{component}` onto slot `{active_slot}` (pid={:?})",
                    snap.pid
                );
            }
            return Ok(());
        }
    }
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?
        );
    } else {
        println!("Rolled back `{component}` to active={active_slot}");
    }
    Ok(())
}

fn run_sign(args: &SignArgs) -> Result<(), String> {
    if !(1..=365).contains(&args.valid_days) {
        return Err("`--valid-days` must be between 1 and 365".to_owned());
    }
    let channel: UpdateChannel = args.channel.into();
    let component: ComponentKind = args.component.into();
    let version = ReleaseVersion::from_str(args.version.trim()).map_err(|e| e.to_string())?;
    let platform = args.platform.clone().unwrap_or_else(current_platform);
    let target_os = platform.split('-').next().unwrap_or("");
    let binary_name = component.binary_file_name_for_os(target_os).to_owned();
    let (size_bytes, sha256_hex) = hash_binary_file(&args.binary).map_err(|e| e.to_string())?;
    let seed_hex = read_signing_seed_hex(args.seed_file.as_deref())?;
    let seed = decode_seed_hex(&seed_hex).map_err(|e| e.to_string())?;
    let issued_at = now_unix_secs()?;
    let expires_at = issued_at.saturating_add(args.valid_days * 24 * 60 * 60);
    let payload = ReleaseManifestPayload {
        schema_version: MANIFEST_SCHEMA_VERSION_V2,
        channel,
        component,
        version,
        build_seq: args.build_seq,
        commit_sha: args.commit_sha.trim().to_ascii_lowercase(),
        storage_schema: args.storage_schema,
        platform,
        binary_name,
        sha256_hex,
        size_bytes,
        schema_compatibility: if args.breaking {
            SchemaCompatibility::Breaking
        } else {
            SchemaCompatibility::ExpandContract
        },
        issued_at_unix_secs: issued_at,
        expires_at_unix_secs: expires_at,
        signing_key_id: args.key_id.clone(),
    };
    let manifest = sign_manifest(payload, &seed).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    fs::write(&args.output, &bytes)
        .map_err(|error| format!("unable to write {}: {error}", args.output.display()))?;
    println!("Signed Manifest v2 written to {}", args.output.display());
    Ok(())
}

fn finish_stage_and_maybe_apply(
    root: &Path,
    component: ComponentKind,
    slot_id: &str,
    changed: bool,
    opts: ApplyWaitOptions,
) -> Result<(), String> {
    if opts.apply || opts.wait {
        let request = write_control_request(root, component, ControlAction::ApplyPending)
            .map_err(|e| e.to_string())?;
        if opts.wait {
            let snap =
                wait_for_control_completion(root, component, &request.request_id, opts.timeout_ms)
                    .map_err(|e| e.to_string())?;
            if opts.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "slot_id": slot_id,
                        "changed": changed,
                        "applied": true,
                        "snapshot": snap,
                    }))
                    .map_err(|e| e.to_string())?
                );
            } else {
                println!(
                    "Staged and applied `{slot_id}` (changed={changed}, pid={:?}, restarts={})",
                    snap.pid, snap.restart_count
                );
            }
            return Ok(());
        }
    }
    if opts.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "slot_id": slot_id,
                "changed": changed,
                "applied": opts.apply,
            }))
            .map_err(|e| e.to_string())?
        );
    } else {
        println!("Staged `{slot_id}` (changed={changed})");
    }
    Ok(())
}

fn resolve_component_source(
    component: ComponentKind,
    repo_override: Option<&str>,
) -> Result<ComponentReleaseSource, String> {
    if let Some(raw) = repo_override {
        return ComponentReleaseSource::parse_owner_repo(raw).map_err(|e| e.to_string());
    }
    match component {
        ComponentKind::Core => Ok(ComponentReleaseSource::keeppeek_core()),
        ComponentKind::Vision => Err(
            "no default remote repository is configured for `keeppeek-vision`; pass `--repo <owner/repo>` or use `--channel local`"
                .to_owned(),
        ),
    }
}

fn resolve_trusted_keys(
    channel: UpdateChannel,
    key_id: Option<&str>,
    public_key_hex: Option<&str>,
) -> Result<Vec<TrustedSigningKey>, String> {
    let mut keys = pinned_keys_for_channel(channel);
    if let Some(custom) = parse_optional_key(key_id, public_key_hex)? {
        keys.push(custom);
    }
    Ok(keys)
}

fn parse_optional_key(
    key_id: Option<&str>,
    public_key_hex: Option<&str>,
) -> Result<Option<TrustedSigningKey>, String> {
    match (key_id, public_key_hex) {
        (Some(id), Some(hex)) => Ok(Some(TrustedSigningKey {
            key_id: id.to_owned(),
            public_key_bytes: decode_public_key_hex(hex).map_err(|e| e.to_string())?,
        })),
        (None, None) => Ok(None),
        _ => Err("`--key-id` and `--public-key-hex` must be provided together".to_owned()),
    }
}

fn parse_optional_version(raw: Option<&str>) -> Result<ReleaseVersion, String> {
    raw.map_or_else(
        || Ok(current_supervisor_version()),
        |text| ReleaseVersion::from_str(text.trim()).map_err(|e| e.to_string()),
    )
}

fn read_signing_seed_hex(seed_file: Option<&Path>) -> Result<String, String> {
    if let Some(path) = seed_file {
        return fs::read_to_string(path)
            .map_err(|error| format!("unable to read signing seed {}: {error}", path.display()));
    }
    std::env::var(SIGNING_SEED_ENV).map_err(|_| {
        format!("signing seed not provided; pass `--seed-file <PATH>` or set `{SIGNING_SEED_ENV}`")
    })
}

fn now_unix_secs() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock error: {error}"))
        .map(|duration| duration.as_secs())
}

fn print_slot_summary(label: &str, slot: &keeppeek_supervisor::slots::ComponentSlotState) {
    println!(
        "  {label}: active={} previous={} pending={} release_floor={} main_floor={} quarantined={}",
        slot.active
            .as_ref()
            .map_or_else(|| "none".to_owned(), ToString::to_string),
        slot.previous
            .as_ref()
            .map_or_else(|| "none".to_owned(), ToString::to_string),
        slot.pending
            .as_ref()
            .map_or_else(|| "none".to_owned(), |p| p.slot.to_string()),
        slot.release_floor
            .map_or_else(|| "none".to_owned(), |v| v.to_string()),
        slot.main_floor
            .map_or_else(|| "none".to_owned(), |f| f.to_string()),
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
        assert!(
            Cli::try_parse_from([
                "keeppeek-supervisor",
                "run",
                "--root",
                "sup",
                "--allow-local"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from(["keeppeek-supervisor", "status", "--root", "sup", "--json"])
                .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "keeppeek-supervisor",
                "check",
                "--root",
                "sup",
                "--channel",
                "main",
                "--json"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "keeppeek-supervisor",
                "update",
                "--root",
                "sup",
                "--channel",
                "local",
                "--binary",
                "target/debug/keeppeek",
                "--apply",
                "--wait",
                "--json"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "keeppeek-supervisor",
                "apply",
                "--root",
                "sup",
                "--wait",
                "--json"
            ])
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
                "--channel",
                "release",
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
