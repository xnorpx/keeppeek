use keeppeek::{
    config,
    logging::initialize_global_logging,
    shutdown::{Restart, Shutdown, restart_current_process},
};
use tracing::info;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> anyhow::Result<()> {
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
