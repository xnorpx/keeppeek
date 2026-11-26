use clap::Parser;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use usage_reporting_receiver::{
    aggregate::compute_public_aggregates,
    http::{ReceiverConfig, ReceiverService},
    retention::{DEFAULT_EXPIRY_SWEEP_INTERVAL, run_expiry_pass, spawn_expiry_worker},
    store::ReceiverStore,
};

#[derive(Debug, Parser)]
#[command(name = "usage-reporting-receiver")]
#[command(about = "Privacy-preserving receiver for opt-in KeepPeek usage reporting")]
struct Cli {
    /// Address to bind the HTTP listener behind a TLS reverse proxy.
    #[arg(long, default_value = "127.0.0.1:8091")]
    bind: String,

    /// Path to the durable SQLite/Turso database file.
    #[arg(long, default_value = "usage-reporting-receiver.db")]
    database: PathBuf,

    /// Optional environment variable holding the bearer token for /v1/admin/* endpoints.
    #[arg(long)]
    admin_token_env: Option<String>,

    /// Export the current public k>=10 aggregate JSON to a file and exit.
    #[arg(long)]
    export_aggregates: Option<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let cli = Cli::parse();

    let mut store = ReceiverStore::open(&cli.database)?;
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        });
    let _ = run_expiry_pass(&mut store, now_secs);

    if let Some(output_path) = cli.export_aggregates {
        let reports = store.active_reports(now_secs)?;
        let dataset = compute_public_aggregates(&reports, now_secs);
        let json = serde_json::to_string_pretty(&dataset)?;
        std::fs::write(&output_path, format!("{json}\n"))?;
        return Ok(());
    }

    let mut config = ReceiverConfig::default();
    if let Some(env_name) = cli.admin_token_env
        && let Ok(token) = std::env::var(env_name)
        && !token.trim().is_empty()
    {
        config = config.with_admin_token(token.trim());
    }

    let shared_store = Arc::new(Mutex::new(store));
    let shutdown = Arc::new(AtomicBool::new(false));
    let _expiry_handle = spawn_expiry_worker(
        Arc::clone(&shared_store),
        DEFAULT_EXPIRY_SWEEP_INTERVAL,
        Arc::clone(&shutdown),
    );
    let service = Arc::new(ReceiverService::new(shared_store, config));
    let bind = cli.bind.clone();
    tracing::info!(bind = %bind, "usage-reporting-receiver listening");
    rouille::start_server(bind, move |request| service.handle(request));
    #[allow(unreachable_code)]
    {
        shutdown.store(true, Ordering::Release);
        Ok(())
    }
}
