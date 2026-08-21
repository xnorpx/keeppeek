use base64::Engine;
use clap::Args;
use keeppeek::{
    cameras,
    cameras::{
        CameraBackend, CameraCapabilities, CameraConfig, CameraPorts, CameraTransport, DeviceInfo,
        DiscoveredCamera, ImagingSettings, MediaProfile, PtzInfo, reolink::ReolinkClient,
    },
    config,
};
use onvif::soap::client::{AuthType, ClientBuilder, Credentials};
use schema::devicemgmt;
use serde::Serialize;
use sha1::{Digest, Sha1};
use std::{
    collections::{BTreeMap, HashMap},
    net::IpAddr,
    path::PathBuf,
    time::Duration,
};
use ureq::Agent;
use url::Url;

#[derive(Args, Debug)]
pub struct Cli {
    /// Usernames to try (repeatable)
    #[arg(short = 'u', long = "username")]
    usernames: Vec<String>,

    /// Passwords to try (repeatable)
    #[arg(short = 'p', long = "password")]
    passwords: Vec<String>,

    /// Load credential candidates from an existing KeepPeek configuration (repeatable)
    #[arg(long = "credentials-from", value_name = "PATH")]
    credentials_from: Vec<PathBuf>,

    /// Extra /24 subnets to scan (third octet, repeatable, 0-255)
    #[arg(short = 's', long = "subnet")]
    subnets: Vec<u8>,

    /// Output TOML file path
    #[arg(
        short = 'o',
        long = "output",
        default_value_os_t = config::config_dir().join("discovered.toml")
    )]
    output: PathBuf,

    /// Info output file path (detailed camera capabilities)
    #[arg(
        short = 'i',
        long = "info",
        default_value_os_t = config::config_dir().join("cameras_info.toml")
    )]
    info_output: PathBuf,

    /// ONVIF ports to probe (overrides defaults)
    #[arg(long = "onvif-ports", value_delimiter = ',')]
    onvif_ports: Vec<u16>,
}

const DEFAULT_ONVIF_PORTS: &[u16] = &[
    80, 443, 554, 2020, 5000, 8000, 8080, 8443, 8554, 8899, 10080,
];

const AUTH_TIMEOUT: Duration = Duration::from_secs(5);

const ONVIF_GET_DEVICE_INFO_TEMPLATE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope"
            xmlns:tds="http://www.onvif.org/ver10/device/wsdl"
            xmlns:wsse="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd"
            xmlns:wsu="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd">
  <s:Header>
    <wsse:Security>
      <wsse:UsernameToken>
        <wsse:Username>{USERNAME}</wsse:Username>
        <wsse:Password Type="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordDigest">{DIGEST}</wsse:Password>
        <wsse:Nonce EncodingType="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-soap-message-security-1.0#Base64Binary">{NONCE}</wsse:Nonce>
        <wsu:Created>{CREATED}</wsu:Created>
      </wsse:UsernameToken>
    </wsse:Security>
  </s:Header>
  <s:Body>
    <tds:GetDeviceInformation/>
  </s:Body>
</s:Envelope>"#;

const ONVIF_GET_DEVICE_INFO_NO_AUTH: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope"
            xmlns:tds="http://www.onvif.org/ver10/device/wsdl">
  <s:Body>
    <tds:GetDeviceInformation/>
  </s:Body>
</s:Envelope>"#;

fn sanitize_toml_key(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let s = s.trim_matches('_').to_string();
    if s.is_empty() {
        "camera".to_string()
    } else {
        s
    }
}

#[derive(Serialize)]
struct InfoOutput {
    cameras: Vec<CameraInfoEntry>,
}

#[derive(Serialize)]
struct CameraInfoEntry {
    ip: IpAddr,
    brand: String,
    name: Option<String>,
    model: Option<String>,
    discovery_sources: Vec<String>,
    device: Option<DeviceInfo>,
    hostname: Option<String>,
    mac_address: Option<String>,
    ports: Option<CameraPorts>,
    capabilities: Option<CameraCapabilities>,
    profiles: Option<Vec<MediaProfile>>,
    ptz: Option<PtzInfo>,
    imaging: Option<ImagingSettings>,
    storage: Option<toml::Value>,
    ai_state: Option<toml::Value>,
    isp: Option<toml::Value>,
    channel_status: Option<toml::Value>,
    system_time: Option<toml::Value>,
    rtsp_urls: Option<toml::Value>,
    onvif_services: Option<Vec<String>>,
}

fn json_to_toml(v: &serde_json::Value) -> Option<toml::Value> {
    match v {
        serde_json::Value::Null => None,
        serde_json::Value::Bool(b) => Some(toml::Value::Boolean(*b)),
        serde_json::Value::Number(n) => n.as_i64().map_or_else(
            || {
                n.as_f64().map_or_else(
                    || Some(toml::Value::String(n.to_string())),
                    |value| Some(toml::Value::Float(value)),
                )
            },
            |value| Some(toml::Value::Integer(value)),
        ),
        serde_json::Value::String(s) => Some(toml::Value::String(s.clone())),
        serde_json::Value::Array(arr) => {
            let items: Vec<toml::Value> = arr.iter().filter_map(json_to_toml).collect();
            if items.is_empty() {
                return None;
            }
            // TOML requires homogeneous arrays
            let first_type = std::mem::discriminant(&items[0]);
            if items
                .iter()
                .all(|item| std::mem::discriminant(item) == first_type)
            {
                Some(toml::Value::Array(items))
            } else {
                // Mixed types: fall back to JSON string
                Some(toml::Value::String(
                    serde_json::to_string(v).unwrap_or_default(),
                ))
            }
        }
        serde_json::Value::Object(obj) => {
            let mut table = toml::map::Map::new();
            for (k, v) in obj {
                if let Some(tv) = json_to_toml(v) {
                    table.insert(k.clone(), tv);
                }
            }
            if table.is_empty() {
                None
            } else {
                Some(toml::Value::Table(table))
            }
        }
    }
}

fn build_onvif_auth_envelope(username: &str, password: &str) -> String {
    let nonce_bytes: [u8; 16] = rand::random();

    let created = chrono_lite_now();

    // WS-Security PasswordDigest = Base64(SHA-1(nonce + created + password))
    let mut hasher = Sha1::new();
    hasher.update(nonce_bytes);
    hasher.update(created.as_bytes());
    hasher.update(password.as_bytes());
    let digest_bytes = hasher.finalize();

    let b64 = base64::engine::general_purpose::STANDARD;
    let nonce_b64 = b64.encode(nonce_bytes);
    let digest_b64 = b64.encode(digest_bytes);

    ONVIF_GET_DEVICE_INFO_TEMPLATE
        .replace("{USERNAME}", username)
        .replace("{DIGEST}", &digest_b64)
        .replace("{NONCE}", &nonce_b64)
        .replace("{CREATED}", &created)
}

fn chrono_lite_now() -> String {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();

    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    let (year, month, day) = epoch_days_to_ymd(days);

    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

const fn epoch_days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from Howard Hinnant's date algorithms
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

struct AuthResult {
    ip: IpAddr,
    username: String,
    password: String,
    /// ONVIF port discovered during auth or port probing.
    onvif_port: Option<u16>,
    /// How credentials were verified.
    method: &'static str,
}

fn http_agent(timeout: Duration) -> Agent {
    Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .build()
        .into()
}

fn test_camera_credentials(
    cam: &DiscoveredCamera,
    usernames: &[String],
    passwords: &[String],
    onvif_ports: &[u16],
    http_agent: &Agent,
) -> Option<AuthResult> {
    let ip = cam.ip;

    // Phase 1: Try Reolink HTTP API login (fast, works for Reolink cameras)
    if cam.brand == "reolink" {
        for user in usernames {
            for pass in passwords {
                if try_reolink_login(ip, user, pass) {
                    tracing::info!("  [reolink] {}  credentials OK: {}:***", ip, user);
                    let onvif_port =
                        probe_onvif_ports_auth(ip, onvif_ports, user, pass, http_agent);

                    return Some(AuthResult {
                        ip,
                        username: user.clone(),
                        password: pass.clone(),
                        onvif_port,
                        method: "reolink",
                    });
                }
            }
        }
    }

    // Phase 2: Try ONVIF WS-Security auth on all candidate ports
    for user in usernames {
        for pass in passwords {
            for &port in onvif_ports {
                if try_onvif_auth(ip, port, user, pass, http_agent) {
                    tracing::info!("  [onvif:{}] {}  credentials OK: {}:***", port, ip, user);
                    return Some(AuthResult {
                        ip,
                        username: user.clone(),
                        password: pass.clone(),
                        onvif_port: Some(port),
                        method: "onvif",
                    });
                }
            }
        }
    }

    // Phase 3: Fallback — find any responding ONVIF port (no auth)
    let onvif_port = probe_onvif_port_no_auth(ip, onvif_ports, http_agent);
    if let Some(port) = onvif_port {
        tracing::info!(
            "  [onvif] {}  ONVIF port {} detected (no working credentials)",
            ip,
            port
        );
    }

    None
}

fn try_reolink_login(ip: IpAddr, username: &str, password: &str) -> bool {
    let mut client = ReolinkClient::new(ip);
    match client.login(username, password) {
        Ok(()) => {
            client.logout().ok();
            true
        }
        Err(_) => false,
    }
}

fn try_onvif_auth(ip: IpAddr, port: u16, username: &str, password: &str, agent: &Agent) -> bool {
    let url = format!("http://{ip}:{port}/onvif/device_service");
    let envelope = build_onvif_auth_envelope(username, password);

    let response = match agent
        .post(&url)
        .header("Content-Type", "application/soap+xml; charset=utf-8")
        .send(envelope)
    {
        Ok(r) => r,
        Err(_) => return false,
    };

    let mut response_body = response.into_body();
    let body = match response_body.read_to_string() {
        Ok(b) => b,
        Err(_) => return false,
    };

    body.contains("GetDeviceInformationResponse")
}

fn probe_onvif_ports_auth(
    ip: IpAddr,
    ports: &[u16],
    username: &str,
    password: &str,
    agent: &Agent,
) -> Option<u16> {
    ports
        .iter()
        .copied()
        .find(|port| try_onvif_auth(ip, *port, username, password, agent))
}

fn probe_onvif_port_no_auth(ip: IpAddr, ports: &[u16], agent: &Agent) -> Option<u16> {
    ports.iter().copied().find(|port| {
        let url = format!("http://{ip}:{port}/onvif/device_service");
        let Ok(response) = agent
            .post(&url)
            .header("Content-Type", "application/soap+xml; charset=utf-8")
            .send(ONVIF_GET_DEVICE_INFO_NO_AUTH)
        else {
            return false;
        };
        let mut response_body = response.into_body();
        let Ok(body) = response_body.read_to_string() else {
            return false;
        };
        body.contains("Envelope") || body.contains("onvif") || body.contains("ONVIF")
    })
}

fn gather_camera_info(
    cam: &DiscoveredCamera,
    auth: Option<&AuthResult>,
    onvif_ports: &[u16],
) -> CameraInfoEntry {
    let mut entry = CameraInfoEntry {
        ip: cam.ip,
        brand: cam.brand.to_string(),
        name: cam.name.clone(),
        model: cam.model.clone(),
        discovery_sources: cam.sources.iter().map(|s| s.to_string()).collect(),
        device: None,
        hostname: None,
        mac_address: None,
        ports: None,
        capabilities: None,
        profiles: None,
        ptz: None,
        imaging: None,
        storage: None,
        ai_state: None,
        isp: None,
        channel_status: None,
        system_time: None,
        rtsp_urls: None,
        onvif_services: None,
    };

    let Some(auth) = auth else {
        return entry;
    };

    if cam.brand == "reolink" {
        let config = CameraConfig {
            ip: cam.ip,
            name: None,
            display_name: None,
            manufacturer: None,
            username: auth.username.clone(),
            password: auth.password.clone(),
            onvif_port: auth.onvif_port,
            http_port: None,
            main_rtsp_url: None,
            sub_rtsp_url: None,
            uid: None,
            backend: CameraBackend::Auto,
            transport: CameraTransport::Tcp,
        };

        match ReolinkClient::connect(&config) {
            Ok((client, camera)) => {
                entry.device = Some(camera.device);
                entry.hostname = camera.hostname;
                entry.mac_address = camera.mac_address;
                entry.ports = Some(camera.ports);
                entry.capabilities = Some(camera.capabilities);
                entry.profiles = Some(camera.profiles);
                entry.ptz = camera.ptz;
                entry.imaging = camera.imaging;

                // Extra queries — tolerate individual failures
                if let Ok(v) = client.get_hdd_info() {
                    entry.storage = json_to_toml(&v);
                }
                if let Ok(v) = client.get_ai_state(0) {
                    entry.ai_state = json_to_toml(&v);
                }
                if let Ok(v) = client.get_isp(0) {
                    entry.isp = json_to_toml(&v);
                }
                if let Ok(v) = client.get_channel_status() {
                    entry.channel_status = json_to_toml(&v);
                }
                if let Ok(v) = client.get_time() {
                    entry.system_time = json_to_toml(&v);
                }
                if let Ok(v) = client.get_rtsp_url(0) {
                    entry.rtsp_urls = json_to_toml(&v);
                }

                tracing::info!(
                    "  [reolink] {}  queried device info, ports, capabilities, profiles, etc.",
                    cam.ip
                );
            }
            Err(e) => {
                tracing::warn!("failed to connect to Reolink camera {}: {}", cam.ip, e);
            }
        }
    }

    let onvif_port = auth.onvif_port.map_or_else(
        || {
            let mut found = None;
            for &port in onvif_ports {
                if query_onvif_services(cam.ip, port, &auth.username, &auth.password)
                    .map(|s| !s.is_empty())
                    .unwrap_or(false)
                {
                    found = Some(port);
                    break;
                }
            }
            found
        },
        Some,
    );

    if let Some(port) = onvif_port {
        let url = Url::parse(&format!("http://{}:{}/onvif/device_service", cam.ip, port)).unwrap();
        let client = ClientBuilder::new(&url)
            .credentials(Some(Credentials {
                username: auth.username.clone(),
                password: auth.password.clone(),
            }))
            .auth_type(AuthType::Any)
            .timeout(Duration::from_secs(10))
            .build();

        if let Ok(resp) = devicemgmt::get_services(
            &client,
            &devicemgmt::GetServices {
                include_capability: false,
            },
        ) {
            let services: Vec<String> = resp.service.iter().map(|s| s.namespace.clone()).collect();
            if !services.is_empty() {
                tracing::info!(
                    "  [onvif:{}] {}  services: {}",
                    port,
                    cam.ip,
                    services.join(", ")
                );
                entry.onvif_services = Some(services);
            }
        }

        if entry.hostname.is_none() {
            let hostname = devicemgmt::get_hostname(&client, &devicemgmt::GetHostname {})
                .ok()
                .and_then(|r| {
                    let name = r.hostname_information.name;
                    match name {
                        Some(n) if !n.is_empty() => Some(n),
                        _ => None,
                    }
                });

            let scope_name = devicemgmt::get_scopes(&client, &devicemgmt::GetScopes {})
                .ok()
                .and_then(|r| {
                    for scope in &r.scopes {
                        let uri = &scope.scope_item;
                        if let Some(name) = uri.strip_prefix("onvif://www.onvif.org/name/") {
                            let decoded = name.replace("%20", " ");
                            if !decoded.is_empty() {
                                return Some(decoded);
                            }
                        }
                    }
                    None
                });

            entry.hostname = scope_name.or(hostname);
            if let Some(ref h) = entry.hostname {
                tracing::info!("  [onvif:{}] {}  hostname: {}", port, cam.ip, h);
            }
        }

        if entry.device.is_none()
            && let Ok(dev) =
                devicemgmt::get_device_information(&client, &devicemgmt::GetDeviceInformation {})
        {
            entry.device = Some(DeviceInfo {
                manufacturer: Some(dev.manufacturer),
                model: Some(dev.model),
                firmware_version: Some(dev.firmware_version),
                serial_number: Some(dev.serial_number),
                hardware_id: Some(dev.hardware_id),
                p2p_uid: None,
            });
        }

        if entry.mac_address.is_none()
            && let Ok(resp) =
                devicemgmt::get_network_interfaces(&client, &devicemgmt::GetNetworkInterfaces {})
        {
            for iface in &resp.network_interfaces {
                if let Some(ref info) = iface.info {
                    let mac = &info.hw_address.0;
                    if !mac.is_empty() {
                        entry.mac_address = Some(mac.clone());
                        break;
                    }
                }
            }
        }
    }

    entry
}

fn query_onvif_services(
    ip: IpAddr,
    port: u16,
    username: &str,
    password: &str,
) -> anyhow::Result<Vec<String>> {
    let url = Url::parse(&format!("http://{ip}:{port}/onvif/device_service"))?;

    let client = ClientBuilder::new(&url)
        .credentials(Some(Credentials {
            username: username.to_string(),
            password: password.to_string(),
        }))
        .auth_type(AuthType::Any)
        .timeout(Duration::from_secs(10))
        .build();

    let request = devicemgmt::GetServices {
        include_capability: false,
    };

    let response = devicemgmt::get_services(&client, &request)
        .map_err(|e| anyhow::anyhow!("ONVIF get_services: {e}"))?;

    let services: Vec<String> = response
        .service
        .iter()
        .map(|s| s.namespace.clone())
        .collect();

    Ok(services)
}

pub fn run(cli: Cli) -> anyhow::Result<()> {
    let (usernames, passwords) =
        credential_candidates(&cli.usernames, &cli.passwords, &cli.credentials_from)?;

    let onvif_ports = if cli.onvif_ports.is_empty() {
        DEFAULT_ONVIF_PORTS.to_vec()
    } else {
        cli.onvif_ports.clone()
    };

    if cli.subnets.is_empty() {
        tracing::info!("discovering cameras on local subnet...");
    } else {
        tracing::info!(
            "discovering cameras on local subnet + extra subnets: {:?}",
            cli.subnets
        );
    }

    let discovered = cameras::discover(None, &cli.subnets)?;

    if discovered.is_empty() {
        println!("no cameras found");
        return Ok(());
    }

    println!("\nfound {} camera(s):\n", discovered.len());
    for cam in &discovered {
        println!("  brand:   {}", cam.brand);
        println!("  ip:      {}", cam.ip);
        println!("  name:    {}", cam.name.as_deref().unwrap_or("unknown"));
        println!("  model:   {}", cam.model.as_deref().unwrap_or("unknown"));
        println!("  found:   {}", cam.sources.join(", "));
        for url in &cam.onvif_urls {
            println!("  onvif:   {url}");
        }
        println!();
    }

    let mut results: Vec<AuthResult> = Vec::new();

    if usernames.is_empty() || passwords.is_empty() {
        println!(
            "no credentials provided (-u / -p / --credentials-from), skipping authentication test"
        );
    } else {
        println!(
            "testing {} username(s) x {} password(s) = {} combinations against {} camera(s)...\n",
            usernames.len(),
            passwords.len(),
            usernames.len() * passwords.len(),
            discovered.len(),
        );

        tracing::info!("ONVIF ports to probe: {:?}", onvif_ports);

        let http_agent = http_agent(AUTH_TIMEOUT);
        for cam in &discovered {
            if let Some(result) =
                test_camera_credentials(cam, &usernames, &passwords, &onvif_ports, &http_agent)
            {
                results.push(result);
            }
        }

        if results.is_empty() {
            println!("no cameras authenticated successfully");
        } else {
            println!("\nauthenticated {} camera(s):\n", results.len());
            for r in &results {
                println!(
                    "  {}  user={}  method={}  onvif_port={}",
                    r.ip,
                    r.username,
                    r.method,
                    r.onvif_port
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| "none".to_string())
                );
            }
        }
    }

    let auth_map: HashMap<IpAddr, AuthResult> = results.into_iter().map(|r| (r.ip, r)).collect();

    let mut configs: Vec<CameraConfig> = discovered
        .iter()
        .map(|cam| {
            auth_map.get(&cam.ip).map_or_else(
                || CameraConfig {
                    ip: cam.ip,
                    name: cam.name.clone(),
                    display_name: None,
                    manufacturer: None,
                    username: String::new(),
                    password: String::new(),
                    onvif_port: None,
                    http_port: None,
                    main_rtsp_url: None,
                    sub_rtsp_url: None,
                    uid: None,
                    backend: CameraBackend::Auto,
                    transport: CameraTransport::Tcp,
                },
                |result| CameraConfig {
                    ip: cam.ip,
                    name: cam.name.clone(),
                    display_name: None,
                    manufacturer: None,
                    username: result.username.clone(),
                    password: result.password.clone(),
                    onvif_port: result.onvif_port,
                    http_port: None,
                    main_rtsp_url: None,
                    sub_rtsp_url: None,
                    uid: None,
                    backend: CameraBackend::Auto,
                    transport: CameraTransport::Tcp,
                },
            )
        })
        .collect();

    println!("\ngathering detailed camera information...\n");

    let mut info_entries = Vec::with_capacity(discovered.len());
    for cam in &discovered {
        tracing::info!("querying info for {} ({})", cam.ip, cam.brand);
        let auth = auth_map.get(&cam.ip);
        let entry = gather_camera_info(cam, auth, &onvif_ports);
        info_entries.push(entry);
    }

    for config in &mut configs {
        if let Some(info) = info_entries.iter().find(|e| e.ip == config.ip)
            && let Some(hostname) = &info.hostname
        {
            let model = info
                .device
                .as_ref()
                .and_then(|d| d.model.as_deref())
                .unwrap_or("");
            let is_model = !model.is_empty() && hostname.eq_ignore_ascii_case(model);
            if !is_model {
                config.name = Some(hostname.clone());
            }
        }
        if config.name.is_none() {
            config.name = Some(config.ip.to_string());
        }
    }

    let mut namespaces: BTreeMap<String, BTreeMap<String, toml::Value>> = BTreeMap::new();
    let ns = namespaces.entry("cameras".to_string()).or_default();

    for config in &configs {
        let raw_name = config.name.clone().unwrap_or_else(|| config.ip.to_string());
        let base = sanitize_toml_key(&raw_name);
        let mut key = base.clone();
        let mut counter = 2;
        while ns.contains_key(&key) {
            key = format!("{base}_{counter}");
            counter += 1;
        }
        let mut val = toml::Value::try_from(config)?;
        if let toml::Value::Table(ref mut table) = val {
            table.remove("name");
        }
        ns.insert(key, val);
    }

    let toml_text = toml::to_string_pretty(&namespaces)?;
    config::ensure_config_dir()?;
    config::write_private_file(&cli.output, toml_text.as_bytes())?;
    println!(
        "\nwrote {} camera(s) to {}",
        configs.len(),
        cli.output.display()
    );

    let info_output = InfoOutput {
        cameras: info_entries,
    };

    let info_toml = toml::to_string_pretty(&info_output)?;
    config::write_private_file(&cli.info_output, info_toml.as_bytes())?;
    println!(
        "wrote {} camera info(s) to {}",
        info_output.cameras.len(),
        cli.info_output.display()
    );

    Ok(())
}

fn credential_candidates(
    cli_usernames: &[String],
    cli_passwords: &[String],
    config_paths: &[PathBuf],
) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let mut usernames = Vec::new();
    let mut passwords = Vec::new();
    extend_unique(&mut usernames, cli_usernames.iter().map(String::as_str));
    extend_unique(&mut passwords, cli_passwords.iter().map(String::as_str));

    for config_path in config_paths {
        let configured = config::load_cameras(config_path)?;
        extend_credentials_from_config(&mut usernames, &mut passwords, &configured);
    }

    Ok((usernames, passwords))
}

fn extend_credentials_from_config(
    usernames: &mut Vec<String>,
    passwords: &mut Vec<String>,
    configured: &HashMap<String, Vec<CameraConfig>>,
) {
    extend_unique(
        usernames,
        configured
            .values()
            .flatten()
            .map(|camera| camera.username.as_str()),
    );
    extend_unique(
        passwords,
        configured
            .values()
            .flatten()
            .map(|camera| camera.password.as_str()),
    );
}

fn extend_unique<'a>(values: &mut Vec<String>, candidates: impl Iterator<Item = &'a str>) {
    for candidate in candidates.filter(|candidate| !candidate.is_empty()) {
        if !values.iter().any(|value| value == candidate) {
            values.push(candidate.to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(clap::Parser)]
    struct TestCli {
        #[command(flatten)]
        command: Cli,
    }

    #[test]
    fn default_outputs_are_kept_outside_the_working_directory() {
        let cli = TestCli::try_parse_from(["keeppeek-camera"])
            .unwrap()
            .command;

        assert_eq!(cli.output, config::config_dir().join("discovered.toml"));
        assert_eq!(
            cli.info_output,
            config::config_dir().join("cameras_info.toml")
        );
    }

    fn camera(username: &str, password: &str, ip: [u8; 4]) -> CameraConfig {
        CameraConfig {
            ip: IpAddr::from(ip),
            name: None,
            display_name: None,
            manufacturer: None,
            username: username.to_owned(),
            password: password.to_owned(),
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
    fn config_credentials_are_deduplicated_and_empty_values_are_ignored() {
        let configured = HashMap::from([
            (
                "front".to_owned(),
                vec![
                    camera("admin", "known-one", [192, 168, 1, 10]),
                    camera("admin", "known-two", [192, 168, 1, 11]),
                ],
            ),
            ("empty".to_owned(), vec![camera("", "", [192, 168, 1, 12])]),
        ]);
        let mut usernames = vec!["operator".to_owned()];
        let mut passwords = vec!["known-one".to_owned()];

        extend_credentials_from_config(&mut usernames, &mut passwords, &configured);

        assert_eq!(usernames, ["operator", "admin"]);
        assert_eq!(passwords, ["known-one", "known-two"]);
    }
}
