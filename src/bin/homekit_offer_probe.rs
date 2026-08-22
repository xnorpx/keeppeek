use clap::{Parser, ValueEnum};
use keeppeek::{
    cameras::{self, CameraConfig},
    config,
    homekit::{HomeKitProbeProfile, HomeKitProbeRequestKind, HomeKitService},
    shutdown::Shutdown,
    webrtc::WebRtc,
};
use std::{
    collections::HashMap,
    net::IpAddr,
    path::PathBuf,
    sync::mpsc::{RecvTimeoutError, sync_channel},
    time::{Duration, Instant},
};
use tracing_subscriber::EnvFilter;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Debug, Parser)]
#[command(
    name = "homekit-offer-probe",
    about = "Exit successfully when a HomeKit controller writes WebRTC Solicit Offer"
)]
struct Cli {
    /// Configuration file containing the selected camera and HomeKit settings.
    #[arg(short, long, default_value_os_t = config::config_path())]
    config: PathBuf,

    /// Camera name, display name, or IP address.
    #[arg(long)]
    camera: String,

    /// Accessory profile to advertise.
    #[arg(long, value_enum, default_value_t = ProbeProfile::Webrtc)]
    profile: ProbeProfile,

    /// Maximum time to wait for WebRTC Solicit Offer.
    #[arg(long, default_value_t = 120)]
    timeout_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ProbeProfile {
    Legacy,
    Webrtc,
}

impl From<ProbeProfile> for HomeKitProbeProfile {
    fn from(value: ProbeProfile) -> Self {
        match value {
            ProbeProfile::Legacy => Self::Legacy,
            ProbeProfile::Webrtc => Self::WebRtc,
        }
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,keeppeek::homekit=debug")),
        )
        .init();

    let cli = Cli::parse();
    let config_text = std::fs::read_to_string(&cli.config)?;
    let app_config: config::Config = toml::from_str(&config_text)?;
    let configured = config::load_cameras(&cli.config)?;
    let selected = select_camera_config(&configured, &cli.camera)?;
    let camera_ip = selected.ip;
    let camera_name = selected.display_name().unwrap_or(&cli.camera).to_owned();
    let probe_config = HashMap::from([("probe".to_owned(), vec![selected])]);
    let cameras = cameras::configured_cameras(&probe_config);
    let mut homekit = app_config.homekit;
    homekit.enabled = true;

    let shutdown = Shutdown::new();
    let signal_shutdown = shutdown.clone();
    ctrlc::set_handler(move || signal_shutdown.cancel())
        .map_err(|error| anyhow::anyhow!("unable to install Ctrl+C handler: {error}"))?;
    let (request_sender, request_receiver) = sync_channel(1);
    let webrtc = WebRtc::new();
    let service = HomeKitService::start_probe(
        &homekit,
        &cli.config,
        &cameras,
        webrtc.clone(),
        shutdown.clone(),
        cli.profile.into(),
        request_sender,
    )?
    .ok_or_else(|| anyhow::anyhow!("HomeKit offer probe did not start"))?;

    eprintln!(
        "WAITING: open '{}' ({}) in Apple Home; listening on {}:{}",
        camera_name, camera_ip, homekit.bind, homekit.port
    );
    let deadline = Instant::now() + Duration::from_secs(cli.timeout_seconds);
    let result = loop {
        if shutdown.is_cancelled() {
            break Err(anyhow::anyhow!("HomeKit offer probe was cancelled"));
        }
        let now = Instant::now();
        if now >= deadline {
            break Err(anyhow::anyhow!(
                "controller did not request an offer within {} seconds",
                cli.timeout_seconds
            ));
        }
        let wait = (deadline - now).min(Duration::from_millis(250));
        match request_receiver.recv_timeout(wait) {
            Ok(request) => {
                let matched = matches!(
                    (cli.profile, request.kind),
                    (
                        ProbeProfile::Legacy,
                        HomeKitProbeRequestKind::LegacySetupEndpoints
                    ) | (
                        ProbeProfile::Webrtc,
                        HomeKitProbeRequestKind::WebRtcSolicitOffer
                    )
                );
                if matched {
                    println!(
                        "SUCCESS: controller selected {:?} for '{}' ({})",
                        request.kind, request.name, request.camera_ip
                    );
                    break Ok(());
                }
                break Err(anyhow::anyhow!(
                    "controller selected {:?} instead of {:?}",
                    request.kind,
                    cli.profile
                ));
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                break Err(anyhow::anyhow!("HomeKit offer probe channel disconnected"));
            }
        }
    };

    shutdown.cancel();
    service.join();
    webrtc.shutdown();
    result
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
            || camera
                .display_name
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case(selector))
    });
    let selected = matches
        .next()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("camera '{selector}' was not found in the configuration"))?;
    if matches.next().is_some() {
        anyhow::bail!("camera selector '{selector}' is ambiguous; use its IP address");
    }
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use keeppeek::cameras::{CameraBackend, CameraTransport};

    fn camera(ip: &str, name: &str, display_name: &str) -> CameraConfig {
        CameraConfig {
            ip: ip.parse().unwrap(),
            name: Some(name.to_owned()),
            display_name: Some(display_name.to_owned()),
            manufacturer: None,
            username: "operator".to_owned(),
            password: "secret".to_owned(),
            onvif_port: None,
            http_port: None,
            main_rtsp_url: None,
            sub_rtsp_url: None,
            uid: None,
            backend: CameraBackend::Auto,
            transport: CameraTransport::Tcp,
        }
    }

    #[test]
    fn selects_camera_by_ip_name_or_display_name() {
        let deck = camera("192.0.2.10", "deck", "Deck Camera");
        let configured = HashMap::from([("cameras".to_owned(), vec![deck.clone()])]);

        for selector in ["192.0.2.10", "DECK", "deck camera"] {
            assert_eq!(
                select_camera_config(&configured, selector).unwrap().ip,
                deck.ip
            );
        }
    }
}
