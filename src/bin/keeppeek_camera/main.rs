use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

mod catalog;
mod discover;
mod stream_test;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Debug, Parser)]
#[command(
    name = "keeppeek-camera",
    about = "Inspect the catalog, discover cameras, and test their live streams"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Report ONVIF capability and explicit service-port evidence in the embedded camera catalog.
    Catalog,
    /// Discover cameras, test credentials, and write staged TOML results.
    Discover(discover::Cli),
    /// Stream selected camera profiles to MP4 and report ingress statistics.
    Test(stream_test::Cli),
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,keeppeek=debug")),
        )
        .init();

    match Cli::parse().command {
        Command::Catalog => catalog::run(),
        Command::Discover(command) => discover::run(command),
        Command::Test(command) => stream_test::run(command),
    }
}
