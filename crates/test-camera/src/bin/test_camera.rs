use clap::{Args, Parser, Subcommand, ValueEnum};
use std::{net::Ipv4Addr, path::PathBuf, sync::mpsc, time::Duration};
use test_camera::{
    TestCameraBuilder, Transport,
    seed::{RecordingSeedOptions, seed_recording},
};
use tracing_subscriber::EnvFilter;

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
    about = "Start deterministic cameras or seed deterministic recordings"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start a local RTSP camera.
    Rtsp(CameraArgs),
    /// Start a local Reolink camera.
    ReoProto(CameraArgs),
    /// Seed deterministic recording data for browser tests.
    SeedRecording(SeedRecordingArgs),
}

#[derive(Debug, Args)]
struct CameraArgs {
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

#[derive(Debug, Args)]
struct SeedRecordingArgs {
    #[arg(long)]
    source: PathBuf,

    #[arg(long)]
    recordings: PathBuf,

    #[arg(long)]
    catalog: PathBuf,

    #[arg(long)]
    stream_id: String,

    #[arg(long, default_value_t = 300)]
    duration_seconds: u64,

    #[arg(long, default_value_t = 240)]
    age_seconds: u64,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    match Cli::parse().command {
        Command::Rtsp(command) => serve_camera(command, false),
        Command::ReoProto(command) => serve_camera(command, true),
        Command::SeedRecording(command) => seed_recording(&RecordingSeedOptions {
            source: command.source,
            recordings: command.recordings,
            catalog: command.catalog,
            stream_id: command.stream_id,
            duration: Duration::from_secs(command.duration_seconds),
            age: Duration::from_secs(command.age_seconds),
        }),
    }
}

fn serve_camera(command: CameraArgs, reo_proto: bool) -> anyhow::Result<()> {
    let builder = if reo_proto {
        TestCameraBuilder::reo_proto(&command.main, &command.sub)
    } else {
        TestCameraBuilder::rtsp(&command.main, &command.sub)
    }
    .bind_ip(command.bind_ip)
    .credentials(command.username, command.password)
    .transport(command.transport.into())
    .uid(command.uid);
    let camera = builder.start()?;

    println!("{}", camera.connection().toml_entry(&command.name));
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
