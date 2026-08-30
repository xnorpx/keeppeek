use chrono::{NaiveDate, Utc};
use clap::{Parser, Subcommand};
use onvif::soap::{self, client::AuthType};
use schema::transport;
use tracing::{debug, warn};
use url::Url;

#[derive(Parser)]
#[command(name = "camera", about = "ONVIF camera control tool")]
struct Args {
    #[arg(global = true, long, requires = "password")]
    username: Option<String>,

    #[arg(global = true, long, requires = "username")]
    password: Option<String>,

    /// The device's base URI, typically just to the HTTP root.
    /// The service-specific path (such as `/onvif/device_support`) will be appended to this.
    // Note this is an `Option` because global options can't be required in clap.
    // https://github.com/clap-rs/clap/issues/1546
    #[arg(global = true, long)]
    uri: Option<Url>,

    /// Service specific path
    #[arg(global = true, long, default_value = "onvif/device_service")]
    service_path: String,

    /// Auto fix time gap between PC and the camera
    #[arg(short = 't', long)]
    fix_time: bool,

    /// Authorization type [Any(Default), Digest, UsernameToken]
    #[arg(short = 'a', long, default_value = "Any")]
    auth_type: String,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    GetSystemDateAndTime,

    GetCapabilities,

    /// Gets the capabilities of all known ONVIF services supported by this device.
    GetServiceCapabilities,

    /// Gets RTSP URIs for all profiles, along with a summary of the video/audio streams.
    GetStreamUris,

    /// Gets JPEG URIs for all profiles
    GetSnapshotUris,

    GetHostname,

    // Gets model, firmware, manufacturer and others informations related to the device.
    GetDeviceInformation,

    SetHostname {
        hostname: String,
    },

    // Gets the PTZ status for the primary media profile.
    GetStatus,

    /// Attempts to enable a `vnd.onvif.metadata` RTSP stream with analytics.
    EnableAnalytics,

    /// Gets information about the currently enabled and supported video analytics.
    GetAnalytics,

    // Try to get any possible information
    GetAll,
}

struct Clients {
    devicemgmt: soap::client::Client,
    event: Option<soap::client::Client>,
    deviceio: Option<soap::client::Client>,
    media: Option<soap::client::Client>,
    media2: Option<soap::client::Client>,
    imaging: Option<soap::client::Client>,
    ptz: Option<soap::client::Client>,
    analytics: Option<soap::client::Client>,
}

impl Clients {
    fn new(args: &Args) -> Result<Self, String> {
        let creds = match (args.username.as_ref(), args.password.as_ref()) {
            (Some(username), Some(password)) => Some(soap::client::Credentials {
                username: username.clone(),
                password: password.clone(),
            }),
            (None, None) => None,
            _ => return Err("username and password must be specified together".to_owned()),
        };
        let base_uri = args
            .uri
            .as_ref()
            .ok_or_else(|| "--uri must be specified.".to_string())?;
        let devicemgmt_uri = base_uri.join(&args.service_path).unwrap();
        let auth_type = match args.auth_type.to_ascii_lowercase().as_str() {
            "digest" => AuthType::Digest,
            "usernametoken" => AuthType::UsernameToken,
            _ => AuthType::Any,
        };
        let mut out = Self {
            devicemgmt: soap::client::ClientBuilder::new(&devicemgmt_uri)
                .credentials(creds.clone())
                .auth_type(auth_type.clone())
                .build(),
            imaging: None,
            ptz: None,
            event: None,
            deviceio: None,
            media: None,
            media2: None,
            analytics: None,
        };

        let time_gap = if args.fix_time {
            let device_time =
                schema::devicemgmt::get_system_date_and_time(&out.devicemgmt, &Default::default())?
                    .system_date_and_time;

            if let Some(utc_time) = &device_time.utc_date_time {
                let pc_time = Utc::now();
                let date = &utc_time.date;
                let t = &utc_time.time;
                let device_time =
                    NaiveDate::from_ymd_opt(date.year, date.month as _, date.day as _)
                        .unwrap()
                        .and_hms_opt(t.hour as _, t.minute as _, t.second as _)
                        .unwrap()
                        .and_utc();

                let diff = device_time - pc_time;
                if diff.num_seconds().abs() > 60 {
                    out.devicemgmt.set_fix_time_gap(Some(diff));
                }
                Some(diff)
            } else {
                warn!("GetSystemDateAndTimeResponse doesn't have utc_data_time value!");
                None
            }
        } else {
            None
        };
        let services = schema::devicemgmt::get_services(&out.devicemgmt, &Default::default())?;
        for service in &services.service {
            let service_url = Url::parse(&service.x_addr).map_err(|e| e.to_string())?;
            if !service_url.as_str().starts_with(base_uri.as_str()) {
                return Err(format!(
                    "Service URI {service_url} is not within base URI {base_uri}"
                ));
            }
            let svc = Some(
                soap::client::ClientBuilder::new(&service_url)
                    .credentials(creds.clone())
                    .auth_type(auth_type.clone())
                    .fix_time_gap(time_gap)
                    .build(),
            );
            match service.namespace.as_str() {
                "http://www.onvif.org/ver10/device/wsdl" => {
                    if service_url != devicemgmt_uri {
                        return Err(format!(
                            "advertised device mgmt uri {service_url} not expected {devicemgmt_uri}"
                        ));
                    }
                }
                "http://www.onvif.org/ver10/events/wsdl" => out.event = svc,
                "http://www.onvif.org/ver10/deviceIO/wsdl" => out.deviceio = svc,
                "http://www.onvif.org/ver10/media/wsdl" => out.media = svc,
                "http://www.onvif.org/ver20/media/wsdl" => out.media2 = svc,
                "http://www.onvif.org/ver20/imaging/wsdl" => out.imaging = svc,
                "http://www.onvif.org/ver20/ptz/wsdl" => out.ptz = svc,
                "http://www.onvif.org/ver20/analytics/wsdl" => out.analytics = svc,
                _ => debug!("unknown service: {:?}", service),
            }
        }
        Ok(out)
    }
}

fn get_capabilities(clients: &Clients) {
    match schema::devicemgmt::get_capabilities(&clients.devicemgmt, &Default::default()) {
        Ok(capabilities) => println!("{capabilities:#?}"),
        Err(error) => println!("Failed to fetch capabilities: {error}"),
    }
}

fn get_device_information(clients: &Clients) -> Result<(), transport::Error> {
    let device_information =
        schema::devicemgmt::get_device_information(&clients.devicemgmt, &Default::default())?;
    println!("{device_information:#?}");
    Ok(())
}

fn get_service_capabilities(clients: &Clients) {
    match schema::event::get_service_capabilities(&clients.devicemgmt, &Default::default()) {
        Ok(capability) => println!("devicemgmt: {capability:#?}"),
        Err(error) => println!("Failed to fetch devicemgmt: {error}"),
    }

    if let Some(ref event) = clients.event {
        match schema::event::get_service_capabilities(event, &Default::default()) {
            Ok(capability) => println!("event: {capability:#?}"),
            Err(error) => println!("Failed to fetch event: {error}"),
        }
    }
    if let Some(ref deviceio) = clients.deviceio {
        match schema::event::get_service_capabilities(deviceio, &Default::default()) {
            Ok(capability) => println!("deviceio: {capability:#?}"),
            Err(error) => println!("Failed to fetch deviceio: {error}"),
        }
    }
    if let Some(ref media) = clients.media {
        match schema::event::get_service_capabilities(media, &Default::default()) {
            Ok(capability) => println!("media: {capability:#?}"),
            Err(error) => println!("Failed to fetch media: {error}"),
        }
    }
    if let Some(ref media2) = clients.media2 {
        match schema::event::get_service_capabilities(media2, &Default::default()) {
            Ok(capability) => println!("media2: {capability:#?}"),
            Err(error) => println!("Failed to fetch media2: {error}"),
        }
    }
    if let Some(ref imaging) = clients.imaging {
        match schema::event::get_service_capabilities(imaging, &Default::default()) {
            Ok(capability) => println!("imaging: {capability:#?}"),
            Err(error) => println!("Failed to fetch imaging: {error}"),
        }
    }
    if let Some(ref ptz) = clients.ptz {
        match schema::event::get_service_capabilities(ptz, &Default::default()) {
            Ok(capability) => println!("ptz: {capability:#?}"),
            Err(error) => println!("Failed to fetch ptz: {error}"),
        }
    }
    if let Some(ref analytics) = clients.analytics {
        match schema::event::get_service_capabilities(analytics, &Default::default()) {
            Ok(capability) => println!("analytics: {capability:#?}"),
            Err(error) => println!("Failed to fetch analytics: {error}"),
        }
    }
}

fn get_system_date_and_time(clients: &Clients) {
    let date =
        schema::devicemgmt::get_system_date_and_time(&clients.devicemgmt, &Default::default());
    println!("{date:#?}");
}

fn get_stream_uris(clients: &Clients) -> Result<(), transport::Error> {
    let media_client = clients
        .media
        .as_ref()
        .ok_or_else(|| transport::Error::Other("Client media is not available".into()))?;
    let profiles = schema::media::get_profiles(media_client, &Default::default())?;
    debug!("get_profiles response: {:#?}", &profiles);
    let requests: Vec<_> = profiles
        .profiles
        .iter()
        .map(|p: &schema::onvif::Profile| schema::media::GetStreamUri {
            profile_token: schema::onvif::ReferenceToken(p.token.0.clone()),
            stream_setup: schema::onvif::StreamSetup {
                stream: schema::onvif::StreamType::RtpUnicast,
                transport: schema::onvif::Transport {
                    protocol: schema::onvif::TransportProtocol::Rtsp,
                    tunnel: vec![],
                },
            },
        })
        .collect();

    let responses = requests
        .iter()
        .map(|request| schema::media::get_stream_uri(media_client, request))
        .collect::<Result<Vec<_>, _>>()?;
    for (p, resp) in profiles.profiles.iter().zip(responses.iter()) {
        println!("token={} name={}", p.token.0, p.name.0);
        println!("    {}", resp.media_uri.uri);
        if let Some(ref v) = p.video_encoder_configuration {
            println!(
                "    {:?}, {}x{}",
                v.encoding, v.resolution.width, v.resolution.height
            );
            if let Some(ref r) = v.rate_control {
                println!("    {} fps, {} kbps", r.frame_rate_limit, r.bitrate_limit);
            }
        }
        if let Some(ref a) = p.audio_encoder_configuration {
            println!(
                "    audio: {:?}, {} kbps, {} kHz",
                a.encoding, a.bitrate, a.sample_rate
            );
        }
    }
    Ok(())
}

fn get_snapshot_uris(clients: &Clients) -> Result<(), transport::Error> {
    let media_client = clients
        .media
        .as_ref()
        .ok_or_else(|| transport::Error::Other("Client media is not available".into()))?;
    let profiles = schema::media::get_profiles(media_client, &Default::default())?;
    debug!("get_profiles response: {:#?}", &profiles);
    let requests: Vec<_> = profiles
        .profiles
        .iter()
        .map(|p: &schema::onvif::Profile| schema::media::GetSnapshotUri {
            profile_token: schema::onvif::ReferenceToken(p.token.0.clone()),
        })
        .collect();

    let responses = requests
        .iter()
        .map(|request| schema::media::get_snapshot_uri(media_client, request))
        .collect::<Result<Vec<_>, _>>()?;
    for (p, resp) in profiles.profiles.iter().zip(responses.iter()) {
        println!("token={} name={}", p.token.0, p.name.0);
        println!("    snapshot_uri={}", resp.media_uri.uri);
    }
    Ok(())
}

fn get_hostname(clients: &Clients) -> Result<(), transport::Error> {
    let resp = schema::devicemgmt::get_hostname(&clients.devicemgmt, &Default::default())?;
    debug!("get_hostname response: {:#?}", &resp);
    println!(
        "{}",
        resp.hostname_information
            .name
            .as_deref()
            .unwrap_or("(unset)")
    );
    Ok(())
}

fn set_hostname(clients: &Clients, hostname: String) -> Result<(), transport::Error> {
    schema::devicemgmt::set_hostname(
        &clients.devicemgmt,
        &schema::devicemgmt::SetHostname { name: hostname },
    )?;
    Ok(())
}

fn enable_analytics(clients: &Clients) -> Result<(), transport::Error> {
    let media_client = clients
        .media
        .as_ref()
        .ok_or_else(|| transport::Error::Other("Client media is not available".into()))?;
    let mut config = schema::media::get_metadata_configurations(media_client, &Default::default())?;
    if config.configurations.len() != 1 {
        println!("Expected exactly one analytics config");
        return Ok(());
    }
    let mut c = config.configurations.pop().unwrap();
    let token_str = c.token.0.clone();
    println!("{c:#?}");
    if c.analytics != Some(true) || c.events.is_none() {
        println!("Enabling analytics in metadata configuration {token_str}");
        c.analytics = Some(true);
        c.events = Some(schema::onvif::EventSubscription {
            filter: None,
            subscription_policy: None,
        });
        schema::media::set_metadata_configuration(
            media_client,
            &schema::media::SetMetadataConfiguration {
                configuration: c,
                force_persistence: true,
            },
        )?;
    } else {
        println!("Analytics already enabled in metadata configuration {token_str}");
    }

    let profiles = schema::media::get_profiles(media_client, &Default::default())?;
    let requests: Vec<_> = profiles
        .profiles
        .iter()
        .filter_map(
            |p: &schema::onvif::Profile| match p.metadata_configuration {
                Some(_) => None,
                None => Some(schema::media::AddMetadataConfiguration {
                    profile_token: schema::onvif::ReferenceToken(p.token.0.clone()),
                    configuration_token: schema::onvif::ReferenceToken(token_str.clone()),
                }),
            },
        )
        .collect();
    if !requests.is_empty() {
        println!(
            "Enabling metadata on {}/{} configs",
            requests.len(),
            profiles.profiles.len()
        );
        requests.iter().try_for_each(|request| {
            schema::media::add_metadata_configuration(media_client, request).map(|_| ())
        })?;
    } else {
        println!(
            "Metadata already enabled on {} configs",
            profiles.profiles.len()
        );
    }
    Ok(())
}

fn get_analytics(clients: &Clients) -> Result<(), transport::Error> {
    let media_client = clients
        .media
        .as_ref()
        .ok_or_else(|| transport::Error::Other("Client media is not available".into()))?;
    let config =
        schema::media::get_video_analytics_configurations(media_client, &Default::default())?;

    println!("{config:#?}");
    let c = match config.configurations.first() {
        Some(c) => c,
        None => return Ok(()),
    };
    if let Some(ref a) = clients.analytics {
        let mods = schema::analytics::get_supported_analytics_modules(
            a,
            &schema::analytics::GetSupportedAnalyticsModules {
                configuration_token: schema::onvif::ReferenceToken(c.token.0.clone()),
            },
        )?;
        println!("{mods:#?}");
    }

    Ok(())
}

fn get_status(clients: &Clients) -> Result<(), transport::Error> {
    if let Some(ref ptz) = clients.ptz {
        let media_client = match clients.media.as_ref() {
            Some(client) => client,
            None => {
                return Err(transport::Error::Other(
                    "Client media is not available".into(),
                ));
            }
        };
        let profile = &schema::media::get_profiles(media_client, &Default::default())?.profiles[0];
        let profile_token = schema::onvif::ReferenceToken(profile.token.0.clone());
        let status = &schema::ptz::get_status(ptz, &schema::ptz::GetStatus { profile_token })?;
        println!("ptz status: {status:#?}");
    }
    Ok(())
}

fn main() {
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    let clients = Clients::new(&args).unwrap();

    match args.cmd {
        Cmd::GetSystemDateAndTime => get_system_date_and_time(&clients),
        Cmd::GetCapabilities => get_capabilities(&clients),
        Cmd::GetServiceCapabilities => get_service_capabilities(&clients),
        Cmd::GetStreamUris => get_stream_uris(&clients).unwrap(),
        Cmd::GetSnapshotUris => get_snapshot_uris(&clients).unwrap(),
        Cmd::GetHostname => get_hostname(&clients).unwrap(),
        Cmd::SetHostname { hostname } => set_hostname(&clients, hostname).unwrap(),
        Cmd::GetDeviceInformation => get_device_information(&clients).unwrap(),
        Cmd::EnableAnalytics => enable_analytics(&clients).unwrap(),
        Cmd::GetAnalytics => get_analytics(&clients).unwrap(),
        Cmd::GetStatus => get_status(&clients).unwrap(),
        Cmd::GetAll => {
            get_system_date_and_time(&clients);
            get_capabilities(&clients);
            get_service_capabilities(&clients);
            get_device_information(&clients).unwrap_or_else(|error| {
                println!("Error while fetching device information: {error:#?}");
            });
            get_stream_uris(&clients).unwrap_or_else(|error| {
                println!("Error while fetching stream urls: {error:#?}");
            });
            get_snapshot_uris(&clients).unwrap_or_else(|error| {
                println!("Error while fetching snapshot urls: {error:#?}");
            });
            get_hostname(&clients).unwrap_or_else(|error| {
                println!("Error while fetching hostname: {error:#?}");
            });
            get_analytics(&clients).unwrap_or_else(|error| {
                println!("Error while fetching analytics: {error:#?}");
            });
            get_status(&clients).unwrap_or_else(|error| {
                println!("Error while fetching status: {error:#?}");
            });
        }
    }
}
