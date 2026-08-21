use clap::{Parser, ValueEnum};
use keeppeek::{
    cameras::{self, CameraBackend, CameraConfig, CameraTransport},
    config,
    keeppeek::KeepPeekLoop,
    shutdown::Shutdown,
    storage::{StorageConfig, StorageEngine},
};
use std::{collections::HashMap, net::IpAddr, path::PathBuf, time::Duration};
use tracing_subscriber::EnvFilter;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum StreamChoice {
    Main,
    Sub,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum BackendChoice {
    Auto,
    Retina,
    ReoProto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum RetinaTransportChoice {
    Tcp,
    Udp,
}

impl From<BackendChoice> for CameraBackend {
    fn from(value: BackendChoice) -> Self {
        match value {
            BackendChoice::Auto => Self::Auto,
            BackendChoice::Retina => Self::Retina,
            BackendChoice::ReoProto => Self::ReoProto,
        }
    }
}

impl From<RetinaTransportChoice> for CameraTransport {
    fn from(value: RetinaTransportChoice) -> Self {
        match value {
            RetinaTransportChoice::Tcp => Self::Tcp,
            RetinaTransportChoice::Udp => Self::Udp,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "camera-stream-test",
    about = "Stream selected camera profiles to MP4 and report ingress statistics"
)]
struct Cli {
    /// Configuration file containing the selected camera.
    #[arg(short, long, default_value_os_t = config::config_path())]
    config: PathBuf,

    /// Camera name from the configuration or its IP address.
    #[arg(long)]
    camera: String,

    /// Streams to record. Repeat the option or use a comma-separated value.
    #[arg(
        long = "stream",
        value_enum,
        value_delimiter = ',',
        default_value = "main"
    )]
    streams: Vec<StreamChoice>,

    /// Streaming backend. Auto uses reo-proto for Reolink cameras and Retina otherwise.
    #[arg(long, value_enum)]
    backend: Option<BackendChoice>,

    /// Transport override. By default, use the camera profile setting.
    #[arg(long = "transport", alias = "retina-transport", value_enum)]
    transport: Option<RetinaTransportChoice>,

    /// Recording duration in seconds. Zero records until Ctrl+C.
    #[arg(long, default_value_t = 60)]
    duration: u64,

    /// Directory for recorded MP4 files.
    #[arg(long, default_value = "camera-stream-test-output")]
    output: PathBuf,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,keeppeek=debug")),
        )
        .init();

    let cli = Cli::parse();
    let config_text = std::fs::read_to_string(&cli.config)?;
    let app_config: config::Config = toml::from_str(&config_text)?;
    let configured = config::load_cameras(&cli.config)?;
    let mut selected = select_camera_config(&configured, &cli.camera)?;
    if let Some(backend) = cli.backend {
        selected.backend = backend.into();
    }
    if let Some(transport) = cli.transport {
        selected.transport = transport.into();
    }
    if selected.username.is_empty() {
        anyhow::bail!(
            "camera '{}' has no username in {}; add credentials before streaming",
            selected.name.as_deref().unwrap_or(&cli.camera),
            cli.config.display()
        );
    }
    let selected_name = selected
        .name
        .clone()
        .unwrap_or_else(|| selected.ip.to_string());

    tracing::info!(
        camera = %selected_name,
        ip = %selected.ip,
        streams = ?cli.streams,
        backend = ?selected.backend,
        transport = ?selected.transport,
        duration_secs = cli.duration,
        output = %cli.output.display(),
        "starting camera stream test",
    );

    let selected_configs = HashMap::from([("selected".to_owned(), vec![selected])]);
    let mut queried = cameras::query_cameras(&selected_configs);
    let camera = queried
        .remove(&selected_configs["selected"][0].ip)
        .ok_or_else(|| anyhow::anyhow!("unable to query selected camera"))?;

    let enable_main = cli.streams.contains(&StreamChoice::Main);
    let enable_sub = cli.streams.contains(&StreamChoice::Sub);
    ensure_requested_streams(&camera, enable_main, enable_sub)?;

    let mut storage_config = StorageConfig::from_toml(&app_config.storage);
    storage_config.medium_term_path = cli.output.clone();
    storage_config.long_term_path = cli.output.clone();
    storage_config.short_term_duration = Duration::ZERO;
    storage_config.flush_interval = Duration::ZERO;
    let storage_engine = StorageEngine::start(storage_config);

    let shutdown = Shutdown::new();
    let signal_shutdown = shutdown.clone();
    ctrlc::set_handler(move || signal_shutdown.cancel())
        .map_err(|error| anyhow::anyhow!("unable to install shutdown signal handler: {error}"))?;

    let mut keeppeek = KeepPeekLoop::new(shutdown.clone(), Some(storage_engine.handle()));
    keeppeek.add_camera(&camera, enable_main, enable_sub)?;

    let keeppeek_handle = std::thread::Builder::new()
        .name("stream-test-keeppeek".to_owned())
        .spawn(move || keeppeek.run())
        .expect("failed to spawn stream test coordinator");

    tracing::info!("recording started; ingress statistics are reported every 10 seconds");
    if cli.duration == 0 {
        while !shutdown.wait_timeout(Duration::from_secs(1)) {}
    } else {
        let _ = shutdown.wait_timeout(Duration::from_secs(cli.duration));
        shutdown.cancel();
    }

    if keeppeek_handle.join().is_err() {
        tracing::warn!("stream test coordinator panicked");
    }
    storage_engine.shutdown();
    tracing::info!(output = %cli.output.display(), "recording finalized");
    Ok(())
}

fn select_camera_config(
    configured: &HashMap<String, Vec<CameraConfig>>,
    selector: &str,
) -> anyhow::Result<CameraConfig> {
    let selected_ip = selector.parse::<IpAddr>().ok();
    let mut matches = configured.values().flatten().filter(|camera| {
        selected_ip == Some(camera.ip)
            || camera
                .name
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case(selector))
    });
    let selected = matches
        .next()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("camera '{selector}' was not found in the configuration"))?;
    if matches.next().is_some() {
        anyhow::bail!("camera name '{selector}' is ambiguous; select it by IP address");
    }
    Ok(selected)
}

fn ensure_requested_streams(
    camera: &cameras::Camera,
    enable_main: bool,
    enable_sub: bool,
) -> anyhow::Result<()> {
    if !enable_main && !enable_sub {
        anyhow::bail!("at least one stream must be selected");
    }
    if camera.profiles.is_empty() {
        anyhow::bail!("camera has no stream profiles");
    }
    if enable_sub && camera.profiles.len() < 2 {
        anyhow::bail!("camera does not expose a sub-stream profile");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_config_is_the_default() {
        let cli = Cli::try_parse_from(["camera-stream-test", "--camera", "front"]).unwrap();

        assert_eq!(cli.config, config::config_path());
    }

    fn camera(name: &str, ip: [u8; 4]) -> CameraConfig {
        CameraConfig {
            ip: IpAddr::from(ip),
            name: Some(name.to_owned()),
            display_name: None,
            manufacturer: None,
            username: "user".to_owned(),
            password: "password".to_owned(),
            onvif_port: None,
            http_port: None,
            main_rtsp_url: None,
            sub_rtsp_url: None,
            uid: None,
            backend: CameraBackend::Auto,
            transport: CameraTransport::Tcp,
            streams: Default::default(),
        }
    }

    #[test]
    fn selects_camera_by_name_or_ip() {
        let configured = HashMap::from([(
            "cameras".to_owned(),
            vec![camera("Front", [192, 168, 1, 10])],
        )]);

        assert_eq!(
            select_camera_config(&configured, "front").unwrap().ip,
            IpAddr::from([192, 168, 1, 10])
        );
        assert_eq!(
            select_camera_config(&configured, "192.168.1.10")
                .unwrap()
                .name
                .as_deref(),
            Some("Front")
        );
    }

    #[test]
    fn rejects_missing_camera() {
        let configured = HashMap::new();
        assert!(select_camera_config(&configured, "missing").is_err());
    }
}
