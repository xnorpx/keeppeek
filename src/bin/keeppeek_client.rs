use clap::Parser;
use keeppeek::client::{ClientCli, KeepPeekClient};

fn main() -> anyhow::Result<()> {
    let cli = ClientCli::parse();
    let client = KeepPeekClient::new(&cli.server);

    let ready = client.ready()?;
    println!("ready: {}", ready.ready);

    let health = client.health()?;
    println!("health: {}", health.status);

    Ok(())
}
