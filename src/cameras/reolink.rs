use super::{
    AudioConfig, AudioEncoding, BAICHUAN_PORT, Camera, CameraBrand, CameraCapabilities,
    CameraConfig, CameraPorts, DeviceInfo, DiscoveredCamera, ImagingSettings, IrCutMode,
    MediaProfile, PtzInfo, PtzPreset, VideoConfig, VideoEncoding,
};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    time::{Duration, Instant},
};
use ureq::Agent;
use url::Url;

const HTTP_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CONCURRENT_PROBES: usize = 200;
const BAICHUAN_LISTEN_TIMEOUT: Duration = Duration::from_secs(5);
const GET_ENC_ATTEMPTS: usize = 3;
const GET_ENC_RETRY_DELAY: Duration = Duration::from_millis(100);
/// Baichuan protocol magic (little-endian 0x0abcdef0)
const BAICHUAN_MAGIC: [u8; 4] = [0xf0, 0xde, 0xbc, 0x0a];
const ONVIF_PORT: u16 = 8000;

const ONVIF_GET_DEVICE_INFO: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope"
            xmlns:tds="http://www.onvif.org/ver10/device/wsdl">
  <s:Body>
    <tds:GetDeviceInformation/>
  </s:Body>
</s:Envelope>"#;

pub struct Reolink;

impl CameraBrand for Reolink {
    fn name(&self) -> &'static str {
        "reolink"
    }

    fn claims_device(&self, name: &str, hardware: &str) -> bool {
        let n = name.to_ascii_lowercase();
        let h = hardware.to_ascii_lowercase();
        n.contains("reolink") || h.contains("reolink")
    }

    fn discover_extra(
        &self,
        _already_claimed: &[IpAddr],
        extra_subnets: &[u8],
    ) -> anyhow::Result<Vec<DiscoveredCamera>> {
        let http_res = discover_http(extra_subnets);
        let baichuan_res = discover_baichuan(extra_subnets);
        let onvif_res = discover_onvif_direct(extra_subnets);

        let mut by_ip: HashMap<IpAddr, DiscoveredCamera> = HashMap::new();

        if let Ok(probes) = http_res {
            for probe in probes {
                let entry = by_ip.entry(probe.ip).or_insert_with(|| DiscoveredCamera {
                    ip: probe.ip,
                    brand: "reolink",
                    name: None,
                    model: None,
                    onvif_urls: vec![],
                    sources: vec![],
                });
                entry.name = entry.name.take().or(probe.name);
                entry.model = entry.model.take().or(probe.model);
                if !entry.sources.contains(&"http") {
                    entry.sources.push("http");
                }
            }
        }

        if let Ok(hits) = baichuan_res {
            for hit in hits {
                let entry = by_ip.entry(hit.ip).or_insert_with(|| DiscoveredCamera {
                    ip: hit.ip,
                    brand: "reolink",
                    name: None,
                    model: None,
                    onvif_urls: vec![],
                    sources: vec![],
                });
                entry.model = entry.model.take().or(hit.model);
                if !entry.sources.contains(&"baichuan") {
                    entry.sources.push("baichuan");
                }
            }
        }

        if let Ok(probes) = onvif_res {
            for probe in probes {
                let entry = by_ip.entry(probe.ip).or_insert_with(|| DiscoveredCamera {
                    ip: probe.ip,
                    brand: "reolink",
                    name: None,
                    model: None,
                    onvif_urls: vec![],
                    sources: vec![],
                });
                entry.name = entry.name.take().or(probe.name);
                entry.model = entry.model.take().or(probe.model);
                if !entry.onvif_urls.contains(&probe.onvif_url) {
                    entry.onvif_urls.push(probe.onvif_url);
                }
                if !entry.sources.contains(&"onvif") {
                    entry.sources.push("onvif");
                }
            }
        }

        Ok(by_ip.into_values().collect())
    }
}

struct HttpProbeResult {
    ip: IpAddr,
    name: Option<String>,
    model: Option<String>,
}

#[derive(Deserialize)]
struct DevInfoValue {
    #[serde(alias = "name")]
    name: Option<String>,
    #[serde(alias = "model")]
    model: Option<String>,
}

#[derive(Deserialize)]
struct ApiResponse {
    #[serde(alias = "value")]
    value: Option<DevInfoValue>,
}

fn http_agent(timeout: Duration) -> Agent {
    Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .build()
        .into()
}

fn discover_http(extra_subnets: &[u8]) -> anyhow::Result<Vec<HttpProbeResult>> {
    let networks = match super::network::scan_networks(extra_subnets) {
        Ok(networks) => networks,
        Err(e) => {
            tracing::warn!("could not detect local networks for HTTP probe: {}", e);
            return Ok(vec![]);
        }
    };

    let ips = super::network::scan_targets(&networks);
    tracing::info!(
        "HTTP probe: scanning {} IPs across {} subnet(s)",
        ips.len(),
        networks.len()
    );
    let agent = http_agent(HTTP_TIMEOUT);

    Ok(super::parallel_filter_map(
        ips,
        MAX_CONCURRENT_PROBES,
        |ip| probe_ip(&agent, ip),
    ))
}

pub fn probe_reolink_http(ip: Ipv4Addr) -> bool {
    probe_ip(&http_agent(HTTP_TIMEOUT), ip).is_some()
}

fn probe_ip(agent: &Agent, ip: Ipv4Addr) -> Option<HttpProbeResult> {
    let url = format!("http://{ip}:80/cgi-bin/api.cgi?cmd=GetDevInfo&token=null");

    let response = agent.get(&url).call().ok()?;
    let mut body = response.into_body();
    let body = body.read_to_string().ok()?;

    if !body.contains("rspCode") && !body.contains("DevInfo") && !body.contains("Reolink") {
        return None;
    }

    tracing::debug!("Reolink HTTP probe hit at {}", ip);

    let (name, model) = parse_dev_info(&body);

    Some(HttpProbeResult {
        ip: IpAddr::V4(ip),
        name,
        model,
    })
}

fn parse_dev_info(body: &str) -> (Option<String>, Option<String>) {
    if let Ok(responses) = serde_json::from_str::<Vec<ApiResponse>>(body)
        && let Some(resp) = responses.first()
        && let Some(val) = &resp.value
    {
        return (val.name.clone(), val.model.clone());
    }
    (None, None)
}

/// Build a minimal 20-byte Baichuan header as a discovery probe.
/// Layout: 4 bytes magic + 4 bytes msg_id + 4 bytes body_len + 4 bytes enc_offset + 4 bytes reserved
fn baichuan_discovery_packet() -> [u8; 20] {
    let mut pkt = [0u8; 20];
    pkt[0..4].copy_from_slice(&BAICHUAN_MAGIC);
    pkt[4..8].copy_from_slice(&1u32.to_le_bytes());
    pkt
}

struct BaichuanHit {
    ip: IpAddr,
    model: Option<String>,
}

fn discover_baichuan(extra_subnets: &[u8]) -> anyhow::Result<Vec<BaichuanHit>> {
    let networks = match super::network::scan_networks(extra_subnets) {
        Ok(networks) => networks,
        Err(e) => {
            tracing::warn!(
                "could not detect local networks for Baichuan broadcast: {}",
                e
            );
            return Ok(vec![]);
        }
    };

    tracing::info!(
        "Baichuan broadcast: sending discovery to 255.255.255.255:{} and {} subnet(s)",
        BAICHUAN_PORT,
        networks.len()
    );

    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.set_broadcast(true)?;

    let probe = baichuan_discovery_packet();

    let _ = socket.send_to(
        &probe,
        SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), BAICHUAN_PORT),
    );

    for network in &networks {
        let _ = socket.send_to(
            &probe,
            SocketAddr::new(IpAddr::V4(network.broadcast), BAICHUAN_PORT),
        );
    }

    let mut hits: Vec<BaichuanHit> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut buf = [0u8; 4096];
    let deadline = Instant::now() + BAICHUAN_LISTEN_TIMEOUT;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        socket.set_read_timeout(Some(remaining))?;

        match socket.recv_from(&mut buf) {
            Ok((len, addr)) => {
                let ip = addr.ip();
                if seen.contains(&ip) {
                    continue;
                }

                if len >= 4 && buf[0..4] == BAICHUAN_MAGIC {
                    tracing::debug!("Baichuan UDP response from {} ({} bytes)", ip, len);
                    seen.insert(ip);

                    let model = if len > 20 {
                        parse_baichuan_model(&buf[20..len])
                    } else {
                        None
                    };

                    hits.push(BaichuanHit { ip, model });
                }
            }
            Err(e) => {
                tracing::trace!("Baichuan recv error: {}", e);
                break;
            }
        }
    }

    tracing::info!("Baichuan broadcast: found {} cameras", hits.len());
    Ok(hits)
}

fn parse_baichuan_model(payload: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(payload);
    extract_xml_value(&text, "model").or_else(|| extract_xml_value(&text, "type"))
}

fn extract_xml_value(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    if let Some(start) = text.find(&open) {
        let value_start = start + open.len();
        if let Some(end) = text[value_start..].find(&close) {
            let val = text[value_start..value_start + end].trim().to_string();
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

struct OnvifDirectResult {
    ip: IpAddr,
    name: Option<String>,
    model: Option<String>,
    onvif_url: Url,
}

fn discover_onvif_direct(extra_subnets: &[u8]) -> anyhow::Result<Vec<OnvifDirectResult>> {
    let networks = match super::network::scan_networks(extra_subnets) {
        Ok(networks) => networks,
        Err(e) => {
            tracing::warn!(
                "could not detect local networks for ONVIF direct probe: {}",
                e
            );
            return Ok(vec![]);
        }
    };

    let ips = super::network::scan_targets(&networks);
    tracing::info!(
        "ONVIF direct probe: scanning {} IPs on port {}",
        ips.len(),
        ONVIF_PORT
    );
    let agent = http_agent(HTTP_TIMEOUT);
    let found =
        super::parallel_filter_map(ips, MAX_CONCURRENT_PROBES, |ip| probe_onvif(&agent, ip));

    tracing::info!("ONVIF direct probe: found {} Reolink cameras", found.len());
    Ok(found)
}

fn probe_onvif(agent: &Agent, ip: Ipv4Addr) -> Option<OnvifDirectResult> {
    let url_str = format!("http://{ip}:{ONVIF_PORT}/onvif/device_service");

    let response = agent
        .post(&url_str)
        .header("Content-Type", "application/soap+xml; charset=utf-8")
        .send(ONVIF_GET_DEVICE_INFO)
        .ok()?;
    let mut body = response.into_body();
    let body = body.read_to_string().ok()?;

    if !body.contains("GetDeviceInformationResponse") {
        return None;
    }

    let manufacturer = find_xml_element(&body, "Manufacturer")?;
    if !manufacturer.to_ascii_lowercase().contains("reolink") {
        return None;
    }

    let model = find_xml_element(&body, "Model");
    tracing::debug!(
        "ONVIF direct probe hit at {} (manufacturer: {}, model: {:?})",
        ip,
        manufacturer,
        model
    );

    let onvif_url = Url::parse(&url_str).ok()?;

    Some(OnvifDirectResult {
        ip: IpAddr::V4(ip),
        name: None,
        model,
        onvif_url,
    })
}

fn find_xml_element(text: &str, local_name: &str) -> Option<String> {
    let needle = format!("{local_name}>");
    let pos = text.find(&needle)?;
    let value_start = pos + needle.len();
    let end = text[value_start..].find('<')?;
    let val = text[value_start..value_start + end].trim().to_string();
    if val.is_empty() { None } else { Some(val) }
}

/// PTZ operation for the `PtzCtrl` command.
#[derive(Debug, Clone, Copy)]
pub enum PtzOp {
    Left,
    Right,
    Up,
    Down,
    LeftUp,
    LeftDown,
    RightUp,
    RightDown,
    ZoomIn,
    ZoomOut,
    FocusNear,
    FocusFar,
    Stop,
    /// Go to preset position (requires `id` in param).
    ToPos,
    /// Start a patrol/cruise route (requires `id` in param).
    StartPatrol,
    /// Stop a patrol/cruise route (requires `id` in param).
    StopPatrol,
    /// Auto-scan left-right.
    Auto,
}

impl PtzOp {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Left => "Left",
            Self::Right => "Right",
            Self::Up => "Up",
            Self::Down => "Down",
            Self::LeftUp => "LeftUp",
            Self::LeftDown => "LeftDown",
            Self::RightUp => "RightUp",
            Self::RightDown => "RightDown",
            Self::ZoomIn => "ZoomInc",
            Self::ZoomOut => "ZoomDec",
            Self::FocusNear => "FocusInc",
            Self::FocusFar => "FocusDec",
            Self::Stop => "Stop",
            Self::ToPos => "ToPos",
            Self::StartPatrol => "StartPatrol",
            Self::StopPatrol => "StopPatrol",
            Self::Auto => "Auto",
        }
    }
}

#[derive(Deserialize)]
struct ApiRsp {
    #[expect(dead_code)]
    cmd: Option<String>,
    code: Option<i32>,
    value: Option<Value>,
    error: Option<RspError>,
}

#[derive(Deserialize)]
struct RspError {
    detail: Option<String>,
    #[serde(alias = "rspCode")]
    rsp_code: Option<i32>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RspDevInfo {
    #[expect(dead_code)]
    name: Option<String>,
    model: Option<String>,
    #[serde(alias = "firmVer")]
    firm_ver: Option<String>,
    serial: Option<String>,
    #[serde(alias = "hardVer")]
    hard_ver: Option<String>,
    #[expect(dead_code)]
    channel_num: Option<u32>,
    #[expect(dead_code)]
    audio_num: Option<u32>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RspNetPort {
    http_port: Option<u16>,
    https_port: Option<u16>,
    rtsp_port: Option<u16>,
    onvif_port: Option<u16>,
}

#[derive(Deserialize, Default)]
struct RspLocalLink {
    mac: Option<String>,
}

#[derive(Deserialize, Default)]
struct RspP2p {
    uid: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RspStreamEnc {
    bit_rate: Option<u32>,
    frame_rate: Option<u32>,
    gop: Option<u32>,
    height: Option<u32>,
    width: Option<u32>,
    profile: Option<String>,
    size: Option<String>,
    #[serde(rename = "vType", alias = "videoType")]
    video_type: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RspEnc {
    audio: Option<u32>,
    #[expect(dead_code)]
    channel: Option<u32>,
    main_stream: Option<RspStreamEnc>,
    sub_stream: Option<RspStreamEnc>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RspOsdChannel {
    name: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RspOsd {
    osd_channel: Option<RspOsdChannel>,
}

#[derive(Deserialize, Default)]
struct RspImage {
    bright: Option<u32>,
    contrast: Option<u32>,
    saturation: Option<u32>,
    sharpen: Option<u32>,
}

#[derive(Deserialize, Default)]
struct RspIrLights {
    state: Option<String>,
}

#[derive(Deserialize, Default)]
struct RspPtzPreset {
    enable: Option<u32>,
    id: Option<u32>,
    name: Option<String>,
}

#[derive(Deserialize, Default)]
struct RspMdState {
    state: Option<u32>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct RspPtzCurPos {
    #[serde(alias = "Ppos")]
    ppos: Option<f64>,
    #[serde(alias = "Tpos")]
    tpos: Option<f64>,
}

pub struct ReolinkClient {
    client: Agent,
    ip: IpAddr,
    base_url: String,
    token: Option<String>,
}

impl ReolinkClient {
    pub fn new(ip: IpAddr) -> Self {
        Self::new_with_http_port(ip, None)
    }

    pub fn new_with_http_port(ip: IpAddr, port: Option<u16>) -> Self {
        let client = http_agent(Duration::from_secs(10));
        let host = match ip {
            IpAddr::V4(ip) => ip.to_string(),
            IpAddr::V6(ip) => format!("[{ip}]"),
        };
        let authority = port
            .filter(|port| *port != 80)
            .map_or_else(|| host.clone(), |port| format!("{host}:{port}"));
        let base_url = format!("http://{authority}/cgi-bin/api.cgi");
        Self {
            client,
            ip,
            base_url,
            token: None,
        }
    }

    fn api_call(&self, cmd: &str, param: Option<Value>) -> anyhow::Result<Value> {
        let mut url = format!("{}?cmd={}", self.base_url, cmd);
        if let Some(token) = &self.token {
            url.push_str("&token=");
            url.push_str(token);
        }

        let body = serde_json::json!([{
            "cmd": cmd,
            "action": 0,
            "param": param.unwrap_or_else(|| Value::Object(Default::default())),
        }]);

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .send(body.to_string())?;
        let mut response_body = response.into_body();
        let text = response_body.read_to_string()?;

        let responses: Vec<ApiRsp> = serde_json::from_str(&text).map_err(|e| {
            anyhow::anyhow!(
                "{} parse error: {} (body: {})",
                cmd,
                e,
                &text[..text.len().min(200)]
            )
        })?;

        let rsp = responses
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("empty response for {cmd}"))?;

        if let Some(err) = &rsp.error
            && let Some(code) = err.rsp_code
            && code != 0
        {
            anyhow::bail!(
                "{} error: code={}, detail={}",
                cmd,
                code,
                err.detail.as_deref().unwrap_or("unknown")
            );
        }

        if let Some(code) = rsp.code
            && code != 0
        {
            anyhow::bail!("{cmd} failed with code {code}");
        }

        rsp.value
            .ok_or_else(|| anyhow::anyhow!("no value in {cmd} response"))
    }

    pub fn login(&mut self, username: &str, password: &str) -> anyhow::Result<()> {
        let param = serde_json::json!({
            "User": {
                "userName": username,
                "password": password,
            }
        });
        let value = self.api_call("Login", Some(param))?;

        let token = value
            .get("Token")
            .and_then(|t| t.get("name"))
            .and_then(|n| n.as_str())
            .map(|s| s.to_string());

        self.token = token;
        if self.token.is_none() {
            anyhow::bail!("Login succeeded but no token returned");
        }
        tracing::debug!("Reolink login OK for {}", self.ip);
        Ok(())
    }

    pub fn logout(&mut self) -> anyhow::Result<()> {
        if self.token.is_some() {
            let _ = self.api_call("Logout", None);
            self.token = None;
        }
        Ok(())
    }

    pub fn get_dev_info(&self) -> anyhow::Result<DeviceInfo> {
        let value = self.api_call("GetDevInfo", None)?;
        let info: RspDevInfo =
            serde_json::from_value(value.get("DevInfo").cloned().unwrap_or_default())?;
        Ok(DeviceInfo {
            manufacturer: Some("Reolink".to_string()),
            model: info.model,
            firmware_version: info.firm_ver,
            serial_number: info.serial,
            hardware_id: info.hard_ver,
            p2p_uid: None,
        })
    }

    pub fn get_p2p_uid(&self) -> anyhow::Result<String> {
        let value = self.api_call("GetP2p", None)?;
        let p2p: RspP2p = serde_json::from_value(value.get("P2p").cloned().unwrap_or(value))?;
        p2p.uid
            .filter(|uid| !uid.is_empty())
            .ok_or_else(|| anyhow::anyhow!("no UID in GetP2p"))
    }

    pub fn get_net_port(&self) -> anyhow::Result<CameraPorts> {
        let value = self.api_call("GetNetPort", None)?;
        let p: RspNetPort =
            serde_json::from_value(value.get("NetPort").cloned().unwrap_or_default())?;
        Ok(CameraPorts {
            http: p.http_port,
            https: p.https_port,
            rtsp: p.rtsp_port,
            onvif: p.onvif_port,
        })
    }

    pub fn get_local_link(&self) -> anyhow::Result<String> {
        let value = self.api_call("GetLocalLink", None)?;
        let link: RspLocalLink =
            serde_json::from_value(value.get("LocalLink").cloned().unwrap_or_default())?;
        link.mac
            .ok_or_else(|| anyhow::anyhow!("no MAC in GetLocalLink"))
    }

    /// Returns configured media profiles without changing encoder settings.
    pub fn get_enc(&self, channel: u32) -> anyhow::Result<Vec<MediaProfile>> {
        let param = serde_json::json!({ "channel": channel });
        let value = self.api_call("GetEnc", Some(param))?;
        let enc: RspEnc = serde_json::from_value(value.get("Enc").cloned().unwrap_or_default())?;

        let has_audio = enc.audio.unwrap_or(0) > 0;
        let mut profiles = Vec::new();

        if let Some(main) = enc.main_stream {
            let (w, h) = parse_resolution(&main);
            let encoding = parse_video_encoding(main.video_type.as_deref());
            profiles.push(MediaProfile {
                token: format!("{channel}_main"),
                name: "mainStream".to_string(),
                stream_uri: None,
                snapshot_uri: None,
                video: Some(VideoConfig {
                    encoding,
                    width: w,
                    height: h,
                    framerate: main.frame_rate.unwrap_or(0) as f64,
                    bitrate_kbps: main.bit_rate,
                    quality: None,
                    gov_length: main.gop,
                    h264_profile: main.profile,
                }),
                audio: if has_audio {
                    Some(AudioConfig {
                        encoding: AudioEncoding::AAC,
                        sample_rate: None,
                        bitrate_kbps: None,
                    })
                } else {
                    None
                },
            });
        }

        if let Some(sub) = enc.sub_stream {
            let (w, h) = parse_resolution(&sub);
            let encoding = parse_video_encoding(sub.video_type.as_deref());
            profiles.push(MediaProfile {
                token: format!("{channel}_sub"),
                name: "subStream".to_string(),
                stream_uri: None,
                snapshot_uri: None,
                video: Some(VideoConfig {
                    encoding,
                    width: w,
                    height: h,
                    framerate: sub.frame_rate.unwrap_or(0) as f64,
                    bitrate_kbps: sub.bit_rate,
                    quality: None,
                    gov_length: sub.gop,
                    h264_profile: sub.profile,
                }),
                audio: None,
            });
        }

        Ok(profiles)
    }

    fn get_enc_with_retry(&self, channel: u32) -> anyhow::Result<Vec<MediaProfile>> {
        let mut last_error = None;

        for attempt in 1..=GET_ENC_ATTEMPTS {
            match self.get_enc(channel) {
                Ok(profiles) if profiles.iter().any(|profile| profile.name == "mainStream") => {
                    return Ok(profiles);
                }
                Ok(_) => {
                    last_error = Some(anyhow::anyhow!(
                        "GetEnc returned no main stream profile on attempt {attempt}"
                    ));
                }
                Err(error) => {
                    last_error = Some(error.context(format!("GetEnc attempt {attempt}")));
                }
            }

            if attempt < GET_ENC_ATTEMPTS {
                std::thread::sleep(GET_ENC_RETRY_DELAY);
            }
        }

        Err(last_error.expect("GetEnc retry loop always makes at least one attempt"))
    }

    pub fn get_osd(&self, channel: u32) -> anyhow::Result<String> {
        let param = serde_json::json!({ "channel": channel });
        let value = self.api_call("GetOsd", Some(param))?;
        let osd: RspOsd = serde_json::from_value(value.get("Osd").cloned().unwrap_or_default())?;
        osd.osd_channel
            .and_then(|c| c.name)
            .ok_or_else(|| anyhow::anyhow!("no name in GetOsd"))
    }

    pub fn get_audio_cfg(&self, channel: u32) -> anyhow::Result<AudioConfig> {
        let param = serde_json::json!({ "channel": channel });
        let value = self.api_call("GetAudioCfg", Some(param))?;
        let cfg = value.get("AudioCfg").cloned().unwrap_or_default();

        let encoding_str = cfg.get("audioType").and_then(|v| v.as_str()).unwrap_or("");
        let encoding = match encoding_str.to_ascii_lowercase().as_str() {
            "aac" => AudioEncoding::AAC,
            "g711a" | "g711u" | "g.711" => AudioEncoding::G711,
            "g726" => AudioEncoding::G726,
            other if !other.is_empty() => AudioEncoding::Unknown(other.to_string()),
            _ => AudioEncoding::AAC,
        };

        Ok(AudioConfig {
            encoding,
            sample_rate: cfg
                .get("sampleRate")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32),
            bitrate_kbps: cfg
                .get("bitRate")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32),
        })
    }

    pub fn get_image(&self, channel: u32) -> anyhow::Result<ImagingSettings> {
        let param = serde_json::json!({ "channel": channel });
        let value = self.api_call("GetImage", Some(param))?;
        let img: RspImage =
            serde_json::from_value(value.get("Image").cloned().unwrap_or_default())?;
        Ok(ImagingSettings {
            brightness: img.bright.map(|v| v as f64),
            contrast: img.contrast.map(|v| v as f64),
            saturation: img.saturation.map(|v| v as f64),
            sharpness: img.sharpen.map(|v| v as f64),
            ir_cut_filter: None,
            backlight_compensation: None,
            wide_dynamic_range: None,
            image_stabilization: None,
        })
    }

    pub fn get_ir_lights(&self, channel: u32) -> anyhow::Result<IrCutMode> {
        let param = serde_json::json!({ "channel": channel });
        let value = self.api_call("GetIrLights", Some(param))?;
        let ir: RspIrLights =
            serde_json::from_value(value.get("IrLights").cloned().unwrap_or_default())?;
        Ok(match ir.state.as_deref() {
            Some("Auto") | Some("auto") => IrCutMode::Auto,
            Some("On") | Some("on") => IrCutMode::On,
            Some("Off") | Some("off") => IrCutMode::Off,
            _ => IrCutMode::Auto,
        })
    }

    pub fn get_ptz_presets(&self, channel: u32) -> anyhow::Result<Vec<PtzPreset>> {
        let param = serde_json::json!({ "channel": channel });
        let value = self.api_call("GetPtzPreset", Some(param))?;

        let arr = value
            .get("PtzPreset")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut presets = Vec::new();
        for item in arr {
            let p: RspPtzPreset = serde_json::from_value(item)?;
            if p.enable.unwrap_or(0) > 0 {
                presets.push(PtzPreset {
                    token: p.id.unwrap_or(0).to_string(),
                    name: p.name,
                });
            }
        }
        Ok(presets)
    }

    pub fn get_ability(&self, username: &str) -> anyhow::Result<CameraCapabilities> {
        let param = serde_json::json!({
            "User": { "userName": username }
        });
        let value = self.api_call("GetAbility", Some(param))?;
        let ability = value.get("Ability").cloned().unwrap_or_default();

        let has = |key: &str| -> bool {
            ability
                .get(key)
                .and_then(|v| v.get("permit"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
                > 0
        };

        let chn_has = |key: &str| -> bool {
            ability
                .get("abilityChn")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|ch| ch.get(key))
                .and_then(|v| v.get("permit"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
                > 0
        };

        Ok(CameraCapabilities {
            ptz: has("ptz") || chn_has("ptz"),
            audio: chn_has("audioCfg"),
            events: has("alarm") || chn_has("alarm"),
            recording: has("record") || chn_has("recCfg"),
            analytics: chn_has("aiTrack") || chn_has("ai"),
            imaging: chn_has("image"),
            two_way_audio: chn_has("talkCfg"),
        })
    }

    pub fn get_ai_state(&self, channel: u32) -> anyhow::Result<Value> {
        let param = serde_json::json!({ "channel": channel });
        self.api_call("GetAiState", Some(param))
    }

    pub fn get_md_state(&self, channel: u32) -> anyhow::Result<bool> {
        let param = serde_json::json!({ "channel": channel });
        let value = self.api_call("GetMdState", Some(param))?;
        let st: RspMdState = serde_json::from_value(value)?;
        Ok(st.state.unwrap_or(0) != 0)
    }

    pub fn get_isp(&self, channel: u32) -> anyhow::Result<Value> {
        let param = serde_json::json!({ "channel": channel });
        let value = self.api_call("GetIsp", Some(param))?;
        Ok(value.get("Isp").cloned().unwrap_or_default())
    }

    pub fn set_isp(
        &self,
        channel: u32,
        day_night: Option<&str>,
        hdr: Option<u32>,
        exposure: Option<&str>,
        back_light: Option<&str>,
    ) -> anyhow::Result<()> {
        let mut isp = serde_json::json!({ "channel": channel });
        if let Some(v) = day_night {
            isp["dayNight"] = Value::String(v.to_string());
        }
        if let Some(v) = hdr {
            isp["hdr"] = Value::from(v);
        }
        if let Some(v) = exposure {
            isp["exposure"] = Value::String(v.to_string());
        }
        if let Some(v) = back_light {
            isp["backLight"] = Value::String(v.to_string());
        }
        let param = serde_json::json!({ "Isp": isp });
        self.api_call("SetIsp", Some(param))?;
        Ok(())
    }

    pub fn set_image(
        &self,
        channel: u32,
        brightness: Option<u32>,
        contrast: Option<u32>,
        saturation: Option<u32>,
        sharpness: Option<u32>,
    ) -> anyhow::Result<()> {
        let mut img = serde_json::json!({ "channel": channel });
        if let Some(v) = brightness {
            img["bright"] = Value::from(v);
        }
        if let Some(v) = contrast {
            img["contrast"] = Value::from(v);
        }
        if let Some(v) = saturation {
            img["saturation"] = Value::from(v);
        }
        if let Some(v) = sharpness {
            img["sharpen"] = Value::from(v);
        }
        let param = serde_json::json!({ "Image": img });
        self.api_call("SetImage", Some(param))?;
        Ok(())
    }

    pub fn set_ir_lights(&self, channel: u32, mode: IrCutMode) -> anyhow::Result<()> {
        let state = match mode {
            IrCutMode::Auto => "Auto",
            IrCutMode::On => "On",
            IrCutMode::Off => "Off",
        };
        let param = serde_json::json!({
            "IrLights": { "channel": channel, "state": state }
        });
        self.api_call("SetIrLights", Some(param))?;
        Ok(())
    }

    pub fn set_osd(&self, channel: u32, name: &str) -> anyhow::Result<()> {
        let param = serde_json::json!({
            "Osd": {
                "channel": channel,
                "osdChannel": { "enable": 1, "name": name }
            }
        });
        self.api_call("SetOsd", Some(param))?;
        Ok(())
    }

    pub fn get_rtsp_url(&self, channel: u32) -> anyhow::Result<Value> {
        let param = serde_json::json!({ "channel": channel });
        self.api_call("GetRtspUrl", Some(param))
    }

    pub fn get_white_led(&self, channel: u32) -> anyhow::Result<Value> {
        let param = serde_json::json!({ "channel": channel });
        let value = self.api_call("GetWhiteLed", Some(param))?;
        Ok(value.get("WhiteLed").cloned().unwrap_or_default())
    }

    pub fn set_white_led(
        &self,
        channel: u32,
        state: u32,
        bright: Option<u32>,
        mode: Option<u32>,
    ) -> anyhow::Result<()> {
        let mut led = serde_json::json!({
            "channel": channel,
            "state": state,
        });
        if let Some(v) = bright {
            led["bright"] = Value::from(v);
        }
        if let Some(v) = mode {
            led["mode"] = Value::from(v);
        }
        let param = serde_json::json!({ "WhiteLed": led });
        self.api_call("SetWhiteLed", Some(param))?;
        Ok(())
    }

    pub fn get_power_led(&self, channel: u32) -> anyhow::Result<Value> {
        let param = serde_json::json!({ "channel": channel });
        let value = self.api_call("GetPowerLed", Some(param))?;
        Ok(value.get("PowerLed").cloned().unwrap_or_default())
    }

    pub fn set_power_led(&self, channel: u32, state: &str) -> anyhow::Result<()> {
        let param = serde_json::json!({
            "PowerLed": { "channel": channel, "state": state }
        });
        self.api_call("SetPowerLed", Some(param))?;
        Ok(())
    }

    pub fn get_state_light(&self) -> anyhow::Result<Value> {
        let value = self.api_call("GetStateLight", None)?;
        Ok(value.get("stateLight").cloned().unwrap_or_default())
    }

    pub fn set_state_light(&self, enable: bool) -> anyhow::Result<()> {
        let param = serde_json::json!({
            "stateLight": { "enable": if enable { 1 } else { 0 } }
        });
        self.api_call("SetStateLight", Some(param))?;
        Ok(())
    }

    pub fn get_auto_focus(&self, channel: u32) -> anyhow::Result<Value> {
        let param = serde_json::json!({ "channel": channel });
        let value = self.api_call("GetAutoFocus", Some(param))?;
        Ok(value.get("AutoFocus").cloned().unwrap_or_default())
    }

    /// `disable=true` turns auto-focus off.
    pub fn set_auto_focus(&self, channel: u32, disable: bool) -> anyhow::Result<()> {
        let param = serde_json::json!({
            "AutoFocus": { "channel": channel, "disable": if disable { 1 } else { 0 } }
        });
        self.api_call("SetAutoFocus", Some(param))?;
        Ok(())
    }

    pub fn get_zoom_focus(&self, channel: u32) -> anyhow::Result<Value> {
        let param = serde_json::json!({ "channel": channel });
        let value = self.api_call("GetZoomFocus", Some(param))?;
        Ok(value.get("ZoomFocus").cloned().unwrap_or_default())
    }

    /// `op` should be `"ZoomPos"` or `"FocusPos"`.
    pub fn start_zoom_focus(&self, channel: u32, op: &str, pos: u32) -> anyhow::Result<()> {
        let param = serde_json::json!({
            "ZoomFocus": { "channel": channel, "op": op, "pos": pos }
        });
        self.api_call("StartZoomFocus", Some(param))?;
        Ok(())
    }

    pub fn get_alarm(&self, channel: u32) -> anyhow::Result<Value> {
        let param = serde_json::json!({
            "Alarm": { "channel": channel, "type": "md" }
        });
        let value = self.api_call("GetAlarm", Some(param))?;
        Ok(value.get("Alarm").cloned().unwrap_or_default())
    }

    pub fn set_alarm(&self, channel: u32, enable: bool) -> anyhow::Result<()> {
        let param = serde_json::json!({
            "Alarm": { "channel": channel, "type": "md", "enable": if enable { 1 } else { 0 } }
        });
        self.api_call("SetAlarm", Some(param))?;
        Ok(())
    }

    pub fn get_md_alarm(&self, channel: u32) -> anyhow::Result<Value> {
        let param = serde_json::json!({ "channel": channel });
        let value = self.api_call("GetMdAlarm", Some(param))?;
        Ok(value.get("MdAlarm").cloned().unwrap_or_default())
    }

    pub fn set_md_alarm(&self, channel: u32, sensitivity: u32) -> anyhow::Result<()> {
        let param = serde_json::json!({
            "MdAlarm": {
                "channel": channel,
                "useNewSens": 1,
                "newSens": { "sensDef": sensitivity }
            }
        });
        self.api_call("SetMdAlarm", Some(param))?;
        Ok(())
    }

    pub fn get_ai_alarm(&self, channel: u32, ai_type: &str) -> anyhow::Result<Value> {
        let param = serde_json::json!({ "channel": channel, "ai_type": ai_type });
        let value = self.api_call("GetAiAlarm", Some(param))?;
        Ok(value.get("AiAlarm").cloned().unwrap_or_default())
    }

    pub fn set_ai_alarm(
        &self,
        channel: u32,
        ai_type: &str,
        sensitivity: u32,
    ) -> anyhow::Result<()> {
        let param = serde_json::json!({
            "AiAlarm": {
                "channel": channel,
                "ai_type": ai_type,
                "sensitivity": sensitivity,
            }
        });
        self.api_call("SetAiAlarm", Some(param))?;
        Ok(())
    }

    pub fn get_audio_alarm(&self, channel: u32) -> anyhow::Result<Value> {
        let param = serde_json::json!({ "channel": channel });
        let value = self.api_call("GetAudioAlarmV20", Some(param))?;
        Ok(value)
    }

    pub fn set_audio_alarm(&self, channel: u32, enable: bool) -> anyhow::Result<()> {
        let param = serde_json::json!({
            "Audio": {
                "enable": if enable { 1 } else { 0 },
                "schedule": { "channel": channel }
            }
        });
        self.api_call("SetAudioAlarmV20", Some(param))?;
        Ok(())
    }

    pub fn get_ai_cfg(&self, channel: u32) -> anyhow::Result<Value> {
        let param = serde_json::json!({ "channel": channel });
        let value = self.api_call("GetAiCfg", Some(param))?;
        Ok(value)
    }

    pub fn set_ai_cfg(
        &self,
        channel: u32,
        ai_track: bool,
        smart_track: bool,
    ) -> anyhow::Result<()> {
        let param = serde_json::json!({
            "channel": channel,
            "aiTrack": if ai_track { 1 } else { 0 },
            "bSmartTrack": if smart_track { 1 } else { 0 },
        });
        self.api_call("SetAiCfg", Some(param))?;
        Ok(())
    }

    pub fn get_ptz_patrol(&self, channel: u32) -> anyhow::Result<Value> {
        let param = serde_json::json!({ "channel": channel });
        let value = self.api_call("GetPtzPatrol", Some(param))?;
        Ok(value.get("PtzPatrol").cloned().unwrap_or_default())
    }

    pub fn start_patrol(&self, channel: u32, id: u32) -> anyhow::Result<()> {
        let param = serde_json::json!({
            "channel": channel,
            "op": "StartPatrol",
            "id": id,
            "speed": 32,
        });
        self.api_call("PtzCtrl", Some(param))?;
        Ok(())
    }

    pub fn stop_patrol(&self, channel: u32, id: u32) -> anyhow::Result<()> {
        let param = serde_json::json!({
            "channel": channel,
            "op": "StopPatrol",
            "id": id,
            "speed": 32,
        });
        self.api_call("PtzCtrl", Some(param))?;
        Ok(())
    }

    pub fn get_ptz_guard(&self, channel: u32) -> anyhow::Result<Value> {
        let param = serde_json::json!({ "channel": channel });
        let value = self.api_call("GetPtzGuard", Some(param))?;
        Ok(value.get("PtzGuard").cloned().unwrap_or_default())
    }

    pub fn set_ptz_guard(
        &self,
        channel: u32,
        enable: bool,
        preset_cmd: Option<&str>,
    ) -> anyhow::Result<()> {
        let mut guard = serde_json::json!({
            "channel": channel,
            "benable": if enable { 1 } else { 0 },
        });
        if let Some(cmd) = preset_cmd {
            guard["cmdStr"] = Value::String(cmd.to_string());
        }
        let param = serde_json::json!({ "PtzGuard": guard });
        self.api_call("SetPtzGuard", Some(param))?;
        Ok(())
    }

    pub fn get_ptz_cur_pos(&self, channel: u32) -> anyhow::Result<(f64, f64)> {
        let param = serde_json::json!({
            "PtzCurPos": { "channel": channel }
        });
        let value = self.api_call("GetPtzCurPos", Some(param))?;
        let pos: RspPtzCurPos =
            serde_json::from_value(value.get("PtzCurPos").cloned().unwrap_or_default())?;
        Ok((pos.ppos.unwrap_or(0.0), pos.tpos.unwrap_or(0.0)))
    }

    pub fn goto_preset(&self, channel: u32, preset_id: u32, speed: u32) -> anyhow::Result<()> {
        let param = serde_json::json!({
            "channel": channel,
            "op": "ToPos",
            "id": preset_id,
            "speed": speed,
        });
        self.api_call("PtzCtrl", Some(param))?;
        Ok(())
    }

    pub fn audio_alarm_play(&self, channel: u32, duration: u32) -> anyhow::Result<()> {
        let param = serde_json::json!({
            "alarm_mode": "times",
            "channel": channel,
            "times": duration,
        });
        self.api_call("AudioAlarmPlay", Some(param))?;
        Ok(())
    }

    pub fn audio_alarm_stop(&self, channel: u32) -> anyhow::Result<()> {
        let param = serde_json::json!({
            "alarm_mode": "manul",
            "channel": channel,
            "manual_switch": 0,
        });
        self.api_call("AudioAlarmPlay", Some(param))?;
        Ok(())
    }

    pub fn get_hdd_info(&self) -> anyhow::Result<Value> {
        self.api_call("GetHddInfo", None)
    }

    pub fn get_manual_rec(&self, channel: u32) -> anyhow::Result<Value> {
        let param = serde_json::json!({ "channel": channel });
        self.api_call("GetManualRec", Some(param))
    }

    pub fn set_manual_rec(&self, channel: u32, enable: bool) -> anyhow::Result<()> {
        let param = serde_json::json!({
            "Rec": {
                "channel": channel,
                "enable": if enable { 1 } else { 0 },
            }
        });
        self.api_call("SetManualRec", Some(param))?;
        Ok(())
    }

    /// Times are in Reolink format: `{"year":2024,"mon":1,"day":15,"hour":0,"min":0,"sec":0}`.
    pub fn search(
        &self,
        channel: u32,
        start_time: Value,
        end_time: Value,
    ) -> anyhow::Result<Value> {
        let param = serde_json::json!({
            "Search": {
                "channel": channel,
                "onlyStatus": 0,
                "streamType": "main",
                "StartTime": start_time,
                "EndTime": end_time,
            }
        });
        self.api_call("Search", Some(param))
    }

    pub fn get_push(&self, channel: u32) -> anyhow::Result<Value> {
        let param = serde_json::json!({ "channel": channel });
        self.api_call("GetPushV20", Some(param))
    }

    pub fn set_push(&self, channel: u32, enable: bool) -> anyhow::Result<()> {
        let param = serde_json::json!({
            "Push": {
                "enable": if enable { 1 } else { 0 },
                "schedule": { "channel": channel }
            }
        });
        self.api_call("SetPushV20", Some(param))?;
        Ok(())
    }

    pub fn get_email(&self, channel: u32) -> anyhow::Result<Value> {
        let param = serde_json::json!({ "channel": channel });
        self.api_call("GetEmailV20", Some(param))
    }

    pub fn get_ftp(&self, channel: u32) -> anyhow::Result<Value> {
        let param = serde_json::json!({ "channel": channel });
        self.api_call("GetFtpV20", Some(param))
    }

    pub fn get_rec(&self, channel: u32) -> anyhow::Result<Value> {
        let param = serde_json::json!({ "channel": channel });
        self.api_call("GetRecV20", Some(param))
    }

    pub fn get_webhook(&self, channel: u32) -> anyhow::Result<Value> {
        let param = serde_json::json!({ "channel": channel });
        self.api_call("GetWebHook", Some(param))
    }

    pub fn set_webhook(
        &self,
        channel: u32,
        index: u32,
        url: &str,
        enable: bool,
    ) -> anyhow::Result<()> {
        let param = serde_json::json!({
            "WebHook": {
                "channel": channel,
                "index": index,
                "hookUrl": url,
                "indexEnable": if enable { 1 } else { 0 },
            }
        });
        self.api_call("SetWebHook", Some(param))?;
        Ok(())
    }

    pub fn test_webhook(&self, channel: u32) -> anyhow::Result<()> {
        let param = serde_json::json!({ "channel": channel });
        self.api_call("TestWebHook", Some(param))?;
        Ok(())
    }

    pub fn get_mask(&self, channel: u32) -> anyhow::Result<Value> {
        let param = serde_json::json!({ "channel": channel });
        let value = self.api_call("GetMask", Some(param))?;
        Ok(value.get("Mask").cloned().unwrap_or_default())
    }

    pub fn set_mask(&self, channel: u32, enable: bool) -> anyhow::Result<()> {
        let param = serde_json::json!({
            "Mask": { "channel": channel, "enable": if enable { 1 } else { 0 } }
        });
        self.api_call("SetMask", Some(param))?;
        Ok(())
    }

    pub fn get_channel_status(&self) -> anyhow::Result<Value> {
        self.api_call("GetChannelstatus", None)
    }

    pub fn get_time(&self) -> anyhow::Result<Value> {
        self.api_call("GetTime", None)
    }

    pub fn get_ntp(&self) -> anyhow::Result<Value> {
        self.api_call("GetNtp", None)
    }

    pub fn set_ntp(&self, server: &str, port: u16) -> anyhow::Result<()> {
        let param = serde_json::json!({
            "Ntp": {
                "enable": 1,
                "server": server,
                "port": port,
            }
        });
        self.api_call("SetNtp", Some(param))?;
        Ok(())
    }

    pub fn reboot(&self) -> anyhow::Result<()> {
        self.api_call("Reboot", None)?;
        Ok(())
    }

    /// Returns JPEG bytes.
    pub fn snap(&self, channel: u32) -> anyhow::Result<Vec<u8>> {
        let mut url = format!("{}?cmd=Snap&channel={}", self.base_url, channel);
        if let Some(token) = &self.token {
            url.push_str("&token=");
            url.push_str(token);
        }
        let response = self.client.get(&url).call()?;
        if !response.status().is_success() {
            anyhow::bail!("Snap failed: HTTP {}", response.status());
        }
        let mut body = response.into_body();
        Ok(body.read_to_vec()?)
    }

    pub fn ptz_ctrl(&self, channel: u32, op: PtzOp, speed: u32) -> anyhow::Result<()> {
        let param = serde_json::json!({
            "channel": channel,
            "op": op.as_str(),
            "speed": speed,
        });
        self.api_call("PtzCtrl", Some(param))?;
        Ok(())
    }

    pub fn connect(config: &CameraConfig) -> anyhow::Result<(Self, Camera)> {
        let mut client = Self::new_with_http_port(config.ip, config.http_port);
        client.login(&config.username, &config.password)?;

        let mut dev_info = client.get_dev_info().unwrap_or_default();
        dev_info.p2p_uid = client.get_p2p_uid().ok();
        let ports = client.get_net_port().unwrap_or_default();
        let mac = client.get_local_link().ok();
        let mut profiles = client.get_enc_with_retry(0)?;
        let hostname = client.get_osd(0).ok();
        let image_settings = client.get_image(0).ok();
        let ir_mode = client.get_ir_lights(0).ok();
        let presets = client.get_ptz_presets(0).unwrap_or_default();
        let capabilities = client.get_ability(&config.username).unwrap_or_default();

        if profiles
            .first()
            .is_some_and(|profile| profile.audio.is_some())
            && let Ok(audio) = client.get_audio_cfg(0)
            && let Some(main) = profiles.first_mut()
        {
            main.audio = Some(audio);
        }

        for profile in &profiles {
            if let Some(ref v) = profile.video {
                tracing::info!(
                    ip = %config.ip,
                    stream = %profile.name,
                    encoding = ?v.encoding,
                    resolution = format_args!("{}x{}", v.width, v.height),
                    framerate = v.framerate,
                    bitrate_kbps = ?v.bitrate_kbps,
                    gop = ?v.gov_length,
                    "Reolink GetEnc profile",
                );
            }
        }

        for profile in &mut profiles {
            profile.snapshot_uri = Some(format!(
                "{}?cmd=Snap&channel=0&token={}",
                client.base_url,
                client.token.as_deref().unwrap_or("")
            ));
        }

        let imaging = match (image_settings, ir_mode) {
            (Some(mut img), ir) => {
                img.ir_cut_filter = ir;
                Some(img)
            }
            (None, Some(ir)) => Some(ImagingSettings {
                brightness: None,
                contrast: None,
                saturation: None,
                sharpness: None,
                ir_cut_filter: Some(ir),
                backlight_compensation: None,
                wide_dynamic_range: None,
                image_stabilization: None,
            }),
            (None, None) => None,
        };

        let ptz = if capabilities.ptz || !presets.is_empty() {
            Some(PtzInfo {
                continuous_move: true,
                absolute_move: false,
                relative_move: false,
                home_support: false,
                e_flip: false,
                reverse: false,
                presets,
                preset_tours: false,
            })
        } else {
            None
        };

        let camera = Camera {
            config: config.clone(),
            reported_manufacturer: dev_info.manufacturer.clone(),
            device: dev_info,
            hostname,
            mac_address: mac,
            ports,
            capabilities,
            profiles,
            is_reolink: true,
            ptz,
            imaging,
        };

        Ok((client, camera))
    }
}

fn parse_resolution(stream: &RspStreamEnc) -> (u32, u32) {
    if let (Some(w), Some(h)) = (stream.width, stream.height)
        && w > 0
        && h > 0
    {
        return (w, h);
    }
    if let Some(size) = &stream.size
        && let Some((w_str, h_str)) = size.split_once('*')
        && let (Ok(w), Ok(h)) = (w_str.trim().parse(), h_str.trim().parse())
    {
        return (w, h);
    }
    (0, stream.height.unwrap_or(0))
}

fn parse_video_encoding(codec: Option<&str>) -> VideoEncoding {
    match codec.map(str::trim) {
        Some(codec)
            if codec.eq_ignore_ascii_case("h264")
                || codec.eq_ignore_ascii_case("h.264")
                || codec.eq_ignore_ascii_case("avc") =>
        {
            VideoEncoding::H264
        }
        Some(codec)
            if codec.eq_ignore_ascii_case("h265")
                || codec.eq_ignore_ascii_case("h.265")
                || codec.eq_ignore_ascii_case("hevc") =>
        {
            VideoEncoding::H265
        }
        Some("") | None => VideoEncoding::H264,
        Some(codec) => VideoEncoding::Unknown(codec.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mixed_get_enc_profile_codecs() {
        let response: RspEnc = serde_json::from_value(serde_json::json!({
            "mainStream": { "vType": "h265" },
            "subStream": { "videoType": "H264" }
        }))
        .unwrap();

        let main = response.main_stream.unwrap();
        let sub = response.sub_stream.unwrap();
        assert_eq!(
            parse_video_encoding(main.video_type.as_deref()),
            VideoEncoding::H265
        );
        assert_eq!(
            parse_video_encoding(sub.video_type.as_deref()),
            VideoEncoding::H264
        );
        assert_eq!(parse_video_encoding(None), VideoEncoding::H264);
    }
}
