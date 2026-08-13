use clap::{Parser, ValueEnum};
use std::{net::Ipv4Addr, path::PathBuf, sync::mpsc};
use test_camera::{TestCameraBuilder, Transport};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Protocol {
    Rtsp,
    ReoProto,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum TransportChoice {
    Tcp,
    Udp,
}

impl From<TransportChoice> for Transport {
    fn from(value: TransportChoice) -> Self {
        match value {
            TransportChoice::Tcp => Self::Tcp,
            TransportChoice::Udp => Self::Udp,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "test-camera",
    about = "Start a local RTSP or Reolink test camera"
)]
struct Cli {
    /// Transport protocol exposed by the test camera.
    #[arg(value_enum)]
    protocol: Protocol,

    /// MP4 source used for the main profile. The video track must be H.264 or H.265.
    #[arg(long)]
    main: PathBuf,

    /// MP4 source used for the sub profile. The video track must be H.264 or H.265.
    #[arg(long)]
    sub: PathBuf,

    /// IPv4 address advertised by the camera.
    #[arg(long, default_value = "127.0.0.1")]
    bind_ip: Ipv4Addr,

    /// Username accepted by the camera.
    #[arg(long, default_value = "test")]
    username: String,

    /// Password accepted by the camera.
    #[arg(long, default_value = "test")]
    password: String,

    /// Transport written into the generated camera configuration entry.
    #[arg(long, value_enum, default_value = "tcp")]
    transport: TransportChoice,

    /// Reolink UID used by the generated Baichuan UDP configuration.
    #[arg(long, default_value = "TESTCAMERA0001")]
    uid: String,

    /// Camera name used in the printed TOML configuration entry.
    #[arg(long, default_value = "local")]
    name: String,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let builder = match cli.protocol {
        Protocol::Rtsp => TestCameraBuilder::rtsp(&cli.main, &cli.sub),
        Protocol::ReoProto => TestCameraBuilder::reo_proto(&cli.main, &cli.sub),
    }
    .bind_ip(cli.bind_ip)
    .credentials(cli.username, cli.password)
    .transport(cli.transport.into())
    .uid(cli.uid);
    let camera = builder.start()?;

    println!("{}", camera.connection().toml_entry(&cli.name));
    tracing::info!(
        main = camera.connection().main_stream_url(),
        sub = camera.connection().sub_stream_url(),
        "test camera is ready"
    );

    let (stop, stopped) = mpsc::channel();
    ctrlc::set_handler(move || {
        let _ = stop.send(());
    })?;
    let _ = stopped.recv();
    drop(camera);
    Ok(())
}
