//! Test camera servers for RTSP and Reolink Baichuan client integration.

mod media;
mod onvif;
mod reo;
mod reolink_http;
pub mod seed;
mod web_ui;

use crate::{
    media::VideoSource, onvif::OnvifServer, reo::ReoServer, reolink_http::ReolinkHttpServer,
    web_ui::CameraWebUiServer,
};
use anyhow::{Context, bail};
use retina::server::{Mp4Playback, RtspServer};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};
#[cfg(windows)]
use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};

const BAICHUAN_PORT: u16 = 9000;

#[cfg(windows)]
struct WindowsTimerResolution(u32);

#[cfg(windows)]
impl WindowsTimerResolution {
    fn request(period_ms: u32) -> Option<Self> {
        // SAFETY: timeBeginPeriod accepts any u32 period and has no pointer or lifetime requirements.
        let result = unsafe { timeBeginPeriod(period_ms) };
        if result == 0 {
            Some(Self(period_ms))
        } else {
            None
        }
    }
}

#[cfg(windows)]
impl Drop for WindowsTimerResolution {
    fn drop(&mut self) {
        // SAFETY: this exactly balances a successful timeBeginPeriod call from request.
        unsafe {
            timeEndPeriod(self.0);
        }
    }
}

/// Selects the camera transport exposed by a [`TestCameraBuilder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// Exposes main and sub profiles over RTSP with TCP or UDP RTP.
    Rtsp,
    /// Exposes main and sub streams over Reolink Baichuan TCP and UDP.
    ReoProto,
}

/// P2P endpoints used by a sleeping battery-camera test double.
#[derive(Debug, Clone, Copy)]
pub struct BatteryWakeEndpoint {
    middleman: SocketAddr,
    register: SocketAddr,
}

impl BatteryWakeEndpoint {
    /// Creates endpoints that emulate the local Reolink P2P service.
    pub const fn new(middleman: SocketAddr, register: SocketAddr) -> Self {
        Self {
            middleman,
            register,
        }
    }

    pub(crate) const fn middleman(self) -> SocketAddr {
        self.middleman
    }

    pub(crate) const fn register(self) -> SocketAddr {
        self.register
    }
}

/// Selects the transport rendered into the camera configuration entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// Use TCP interleaving for RTSP or Baichuan TCP for Reo-proto.
    Tcp,
    /// Use UDP unicast RTP for RTSP or Baichuan UDP for Reo-proto.
    Udp,
}

impl Transport {
    const fn config_name(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}

impl Protocol {
    const fn config_backend(self) -> &'static str {
        match self {
            Self::Rtsp => "retina",
            Self::ReoProto => "reo-proto",
        }
    }
}

/// Builds a local test camera from MP4 sources.
///
/// Both sources must contain one H.264 or H.265 video track. They may use
/// different codecs and resolutions, allowing tests to model a main/sub pair.
#[derive(Debug, Clone)]
pub struct TestCameraBuilder {
    protocol: Protocol,
    main_source: PathBuf,
    sub_source: PathBuf,
    bind_ip: Ipv4Addr,
    config_ip: Option<Ipv4Addr>,
    username: String,
    password: String,
    uid: String,
    transport: Transport,
    battery_wake: Option<BatteryWakeEndpoint>,
    isolated_reo_ports: bool,
    realtime_start_at: Option<Duration>,
}

impl TestCameraBuilder {
    /// Creates a builder for a generic RTSP test camera.
    pub fn rtsp(main_source: impl AsRef<Path>, sub_source: impl AsRef<Path>) -> Self {
        Self::new(Protocol::Rtsp, main_source, sub_source)
    }

    /// Creates a builder for a Reolink Baichuan test camera.
    pub fn reo_proto(main_source: impl AsRef<Path>, sub_source: impl AsRef<Path>) -> Self {
        Self::new(Protocol::ReoProto, main_source, sub_source)
    }

    /// Creates a sleeping Reolink battery camera that wakes through local P2P endpoints.
    pub fn battery_reo_proto(
        main_source: impl AsRef<Path>,
        sub_source: impl AsRef<Path>,
        battery_wake: BatteryWakeEndpoint,
    ) -> Self {
        Self::reo_proto(main_source, sub_source).battery_wake(battery_wake)
    }

    fn new(
        protocol: Protocol,
        main_source: impl AsRef<Path>,
        sub_source: impl AsRef<Path>,
    ) -> Self {
        Self {
            protocol,
            main_source: main_source.as_ref().to_path_buf(),
            sub_source: sub_source.as_ref().to_path_buf(),
            bind_ip: Ipv4Addr::LOCALHOST,
            config_ip: None,
            username: "test".to_owned(),
            password: "test".to_owned(),
            uid: "TESTCAMERA0001".to_owned(),
            transport: Transport::Tcp,
            battery_wake: None,
            isolated_reo_ports: false,
            realtime_start_at: None,
        }
    }

    /// Sets the address advertised by the test camera.
    pub const fn bind_ip(mut self, bind_ip: Ipv4Addr) -> Self {
        self.bind_ip = bind_ip;
        self
    }

    /// Sets the camera IP rendered into configuration while services remain on `bind_ip`.
    pub const fn config_ip(mut self, config_ip: Ipv4Addr) -> Self {
        self.config_ip = Some(config_ip);
        self
    }

    /// Paces and loops RTSP media from the latest sync sample at or before `start_at`.
    pub const fn realtime_start_at(mut self, start_at: Duration) -> Self {
        self.realtime_start_at = Some(start_at);
        self
    }

    /// Sets credentials accepted by the ONVIF and Baichuan façades.
    pub fn credentials(mut self, username: impl Into<String>, password: impl Into<String>) -> Self {
        self.username = username.into();
        self.password = password.into();
        self
    }

    /// Sets the transport selected by the generated camera configuration entry.
    pub const fn transport(mut self, transport: Transport) -> Self {
        self.transport = transport;
        self
    }

    /// Sets the Reolink UID written into generated Reo-proto configuration entries.
    pub fn uid(mut self, uid: impl Into<String>) -> Self {
        self.uid = uid.into();
        self
    }

    /// Makes this Reo-proto camera start asleep and wait for a P2P wake packet.
    pub const fn battery_wake(mut self, battery_wake: BatteryWakeEndpoint) -> Self {
        self.battery_wake = Some(battery_wake);
        self
    }

    /// Uses OS-assigned loopback ports for an isolated Reo-proto test camera.
    pub const fn isolated_reo_ports(mut self) -> Self {
        self.isolated_reo_ports = true;
        self
    }

    /// Starts the configured camera and its accompanying ONVIF façade.
    ///
    /// # Errors
    ///
    /// Returns an error when either media source is invalid, an endpoint cannot
    /// bind.
    pub fn start(self) -> anyhow::Result<TestCamera> {
        #[cfg(windows)]
        let timer_resolution = WindowsTimerResolution::request(1);

        let main = VideoSource::from_mp4(&self.main_source).with_context(|| {
            format!("unable to load main source {}", self.main_source.display())
        })?;
        let sub = VideoSource::from_mp4(&self.sub_source)
            .with_context(|| format!("unable to load sub source {}", self.sub_source.display()))?;

        if self.battery_wake.is_some() && self.protocol != Protocol::ReoProto {
            bail!("battery wake simulation requires the Reo-proto camera protocol");
        }
        if self.realtime_start_at.is_some() && self.protocol != Protocol::Rtsp {
            bail!("real-time MP4 start offsets require the RTSP camera protocol");
        }
        if self.config_ip.is_some() && self.protocol != Protocol::Rtsp {
            bail!("a separate configuration IP requires the RTSP camera protocol");
        }

        let (endpoint_ip, main_stream_url, sub_stream_url, baichuan_port, bcudp_port, transport) =
            match self.protocol {
                Protocol::Rtsp => {
                    if let Some(start_at) = self.realtime_start_at {
                        let playback = Mp4Playback::realtime_looping(start_at);
                        let main = RtspServer::from_mp4_on_with_playback(
                            SocketAddr::new(IpAddr::V4(self.bind_ip), 0),
                            &self.main_source,
                            playback,
                        )
                        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                        let sub = RtspServer::from_mp4_on_with_playback(
                            SocketAddr::new(IpAddr::V4(self.bind_ip), 0),
                            &self.sub_source,
                            playback,
                        )
                        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                        let endpoint_ip = ipv4_server_ip(main.address())?;
                        (
                            endpoint_ip,
                            main.url().to_string(),
                            sub.url().to_string(),
                            None,
                            None,
                            ServerTransport::Rtsp {
                                _servers: vec![main, sub],
                            },
                        )
                    } else {
                        let camera = RtspServer::from_mp4_streams_on(
                            SocketAddr::new(IpAddr::V4(self.bind_ip), 0),
                            &self.main_source,
                            &self.sub_source,
                        )
                        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                        let endpoint_ip = ipv4_server_ip(camera.address())?;
                        (
                            endpoint_ip,
                            camera.high_resolution_url().to_string(),
                            camera.low_resolution_url().to_string(),
                            None,
                            None,
                            ServerTransport::Rtsp {
                                _servers: vec![camera],
                            },
                        )
                    }
                }
                Protocol::ReoProto => {
                    let address = SocketAddr::new(
                        IpAddr::V4(self.bind_ip),
                        if self.isolated_reo_ports {
                            0
                        } else {
                            BAICHUAN_PORT
                        },
                    );
                    let camera = ReoServer::start(
                        address,
                        self.username.clone(),
                        self.password.clone(),
                        main.clone(),
                        sub.clone(),
                        self.battery_wake,
                        self.uid.clone(),
                    )?;
                    let baichuan_port = camera.tcp_port();
                    let bcudp_port = camera.primary_udp_port();
                    let stream_port = baichuan_port.unwrap_or(0);
                    let stream_address = SocketAddr::new(IpAddr::V4(self.bind_ip), stream_port);
                    (
                        self.bind_ip,
                        format!("rtsp://{stream_address}/main"),
                        format!("rtsp://{stream_address}/sub"),
                        baichuan_port,
                        Some(bcudp_port),
                        ServerTransport::Reo { _server: camera },
                    )
                }
            };
        let config_ip = self.config_ip.unwrap_or(endpoint_ip);

        let manufacturer = match self.protocol {
            Protocol::Rtsp => "Test Camera",
            Protocol::ReoProto => "Reolink",
        };
        let onvif = OnvifServer::start(
            SocketAddr::new(IpAddr::V4(endpoint_ip), 0),
            onvif::CameraDescription {
                manufacturer: manufacturer.to_owned(),
                model: if self.battery_wake.is_some() {
                    "Argus-Test".to_owned()
                } else {
                    match self.protocol {
                        Protocol::Rtsp => "RTSP Test Camera".to_owned(),
                        Protocol::ReoProto => "RLC-Test".to_owned(),
                    }
                },
                main: onvif::ProfileDescription::from_source(
                    "main",
                    "Main",
                    main.clone(),
                    main_stream_url.clone(),
                ),
                sub: onvif::ProfileDescription::from_source(
                    "sub",
                    "Sub",
                    sub.clone(),
                    sub_stream_url.clone(),
                ),
            },
        )?;
        let reolink_http = match self.protocol {
            Protocol::Rtsp => None,
            Protocol::ReoProto => Some(ReolinkHttpServer::start(
                SocketAddr::new(IpAddr::V4(endpoint_ip), 0),
                self.username.clone(),
                self.password.clone(),
                main,
                sub,
                onvif.address().port(),
            )?),
        };
        let web_ui = match self.protocol {
            Protocol::Rtsp => Some(CameraWebUiServer::start(
                SocketAddr::new(IpAddr::V4(endpoint_ip), 0),
                "Fake Retina Camera".to_owned(),
            )?),
            Protocol::ReoProto => None,
        };
        let connection = ConnectionInfo {
            ip: config_ip,
            endpoint_ip,
            onvif_port: onvif.address().port(),
            http_port: reolink_http
                .as_ref()
                .map(|server| server.address().port())
                .or_else(|| web_ui.as_ref().map(|server| server.address().port())),
            protocol: self.protocol,
            username: self.username,
            password: self.password,
            uid: self.uid,
            transport: self.transport,
            battery: self.battery_wake.is_some(),
            baichuan_port,
            bcudp_port,
            main_stream_url,
            sub_stream_url,
        };

        Ok(TestCamera {
            connection,
            _onvif: onvif,
            _transport: transport,
            _reolink_http: reolink_http,
            _web_ui: web_ui,
            #[cfg(windows)]
            _timer_resolution: timer_resolution,
        })
    }
}

/// A running test camera and the information needed to connect to it.
pub struct TestCamera {
    connection: ConnectionInfo,
    _onvif: OnvifServer,
    _transport: ServerTransport,
    _reolink_http: Option<ReolinkHttpServer>,
    _web_ui: Option<CameraWebUiServer>,
    #[cfg(windows)]
    _timer_resolution: Option<WindowsTimerResolution>,
}

impl TestCamera {
    /// Returns the connection details for this running camera.
    pub const fn connection(&self) -> &ConnectionInfo {
        &self.connection
    }
}

enum ServerTransport {
    Rtsp { _servers: Vec<RtspServer> },
    Reo { _server: ReoServer },
}

fn ipv4_server_ip(address: SocketAddr) -> anyhow::Result<Ipv4Addr> {
    match address.ip() {
        IpAddr::V4(ip) => Ok(ip),
        IpAddr::V6(_) => bail!("Retina test camera returned a non-IPv4 address"),
    }
}

/// Connection information for a running [`TestCamera`].
#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    ip: Ipv4Addr,
    endpoint_ip: Ipv4Addr,
    onvif_port: u16,
    http_port: Option<u16>,
    protocol: Protocol,
    username: String,
    password: String,
    uid: String,
    transport: Transport,
    battery: bool,
    baichuan_port: Option<u16>,
    bcudp_port: Option<u16>,
    main_stream_url: String,
    sub_stream_url: String,
}

impl ConnectionInfo {
    /// Returns the IP address to use in a camera configuration.
    pub const fn ip(&self) -> Ipv4Addr {
        self.ip
    }

    /// Returns the local address hosting the camera services.
    pub const fn endpoint_ip(&self) -> Ipv4Addr {
        self.endpoint_ip
    }

    /// Returns the ONVIF service port to use in a camera configuration.
    pub const fn onvif_port(&self) -> u16 {
        self.onvif_port
    }

    /// Returns the fake Reolink HTTP API port when this is a Reo-proto camera.
    pub const fn http_port(&self) -> Option<u16> {
        self.http_port
    }

    /// Returns whether this camera begins asleep and requires a P2P wake packet.
    pub const fn is_battery(&self) -> bool {
        self.battery
    }

    /// Returns the Baichuan TCP port for a Reo-proto camera.
    pub const fn baichuan_port(&self) -> Option<u16> {
        self.baichuan_port
    }

    /// Returns the primary BCUDP discovery port for a Reo-proto camera.
    pub const fn bcudp_port(&self) -> Option<u16> {
        self.bcudp_port
    }

    /// Returns the main-profile RTSP URI advertised through ONVIF.
    pub fn main_stream_url(&self) -> &str {
        &self.main_stream_url
    }

    /// Returns the sub-profile RTSP URI advertised through ONVIF.
    pub fn sub_stream_url(&self) -> &str {
        &self.sub_stream_url
    }

    /// Renders a TOML entry accepted by the application camera configuration loader.
    pub fn toml_entry(&self, name: &str) -> String {
        let name = name.replace('"', "\\\"");
        let services_share_config_ip = self.ip == self.endpoint_ip;
        let (onvif_port, http_port) = if services_share_config_ip {
            (
                format!("onvif_port = {}\n", self.onvif_port),
                self.http_port
                    .map(|port| format!("http_port = {port}\n"))
                    .unwrap_or_default(),
            )
        } else {
            (String::new(), String::new())
        };
        let uid = matches!(self.protocol, Protocol::ReoProto)
            .then(|| format!("uid = \"{}\"\n", self.uid));
        format!(
            "[test-camera.\"{name}\"]\nip = \"{}\"\nusername = \"{}\"\npassword = \"{}\"\n{onvif_port}{http_port}main_rtsp_url = \"{}\"\nsub_rtsp_url = \"{}\"\nbackend = \"{}\"\ntransport = \"{}\"\n{}",
            self.ip,
            self.username,
            self.password,
            self.main_stream_url,
            self.sub_stream_url,
            self.protocol.config_backend(),
            self.transport.config_name(),
            uid.unwrap_or_default(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::Codec;
    use ::onvif::soap::client::{AuthType, ClientBuilder, Credentials};
    use reo_proto::{
        BcSession, BcUdpConfig, BcUdpDiscovery, BcUdpDiscoveryConfig, BcUdpDiscoveryOutput,
        BcUdpOutput, BcUdpPacket, Command, EncryptionMode, Event, Input, LoginParams, Output,
        StreamSubscription, StreamType, UdpDiscovery,
    };
    use rouille::url::Url;
    use schema::{devicemgmt, media as onvif_media, onvif as onvif_xsd};
    use std::{
        io::{Read, Write},
        net::{Shutdown, TcpStream, UdpSocket},
        sync::mpsc,
        thread,
        time::Instant,
    };
    use tempfile::NamedTempFile;

    const H264_SPS: &[u8] = &[0x67, 0x42, 0x00, 0x1f, 0xe5, 0x88, 0x68, 0x40];
    const H264_PPS: &[u8] = &[0x68, 0xce, 0x3c, 0x80];
    const RUN_SLOW_TESTS_ENV: &str = "KEEPPEEK_RUN_SLOW_TESTS";

    fn slow_tests_enabled() -> bool {
        std::env::var_os(RUN_SLOW_TESTS_ENV).is_some()
    }

    struct BatteryWakeMock {
        endpoint: BatteryWakeEndpoint,
        registered: mpsc::Receiver<SocketAddr>,
        wake: mpsc::Sender<()>,
        stop: mpsc::Sender<()>,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl BatteryWakeMock {
        fn start() -> Self {
            let middleman = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let register = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            middleman.set_nonblocking(true).unwrap();
            register.set_nonblocking(true).unwrap();
            let endpoint = BatteryWakeEndpoint::new(
                middleman.local_addr().unwrap(),
                register.local_addr().unwrap(),
            );
            let (registered_tx, registered) = mpsc::sync_channel(1);
            let (wake, wake_rx) = mpsc::channel();
            let (stop, stop_rx) = mpsc::channel();
            let worker = thread::spawn(move || {
                let mut camera = None;
                let mut buffer = [0u8; 4 * 1024];
                loop {
                    if matches!(
                        stop_rx.try_recv(),
                        Ok(()) | Err(mpsc::TryRecvError::Disconnected)
                    ) {
                        return;
                    }
                    match middleman.recv_from(&mut buffer) {
                        Ok((read, source)) => {
                            let BcUdpPacket::Discovery(request) =
                                BcUdpPacket::decode(&buffer[..read]).unwrap()
                            else {
                                continue;
                            };
                            if request.xml.windows(5).any(|part| part == b"D2M_Q") {
                                let response = p2p_packet(
                                    request.transmission_id,
                                    "<P2P><M2D_Q_R><token>7</token><ac>11</ac></M2D_Q_R></P2P>",
                                );
                                middleman.send_to(&response, source).unwrap();
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(error) => panic!("battery wake middleman failed: {error}"),
                    }
                    match register.recv_from(&mut buffer) {
                        Ok((read, source)) => {
                            let BcUdpPacket::Discovery(request) =
                                BcUdpPacket::decode(&buffer[..read]).unwrap()
                            else {
                                continue;
                            };
                            if request.xml.windows(5).any(|part| part == b"D2R_R") {
                                let response = p2p_packet(
                                    request.transmission_id,
                                    "<P2P><R2D_R_R><rsp>-4</rsp><ac>11</ac></R2D_R_R></P2P>",
                                );
                                register.send_to(&response, source).unwrap();
                            }
                            if request.xml.windows(6).any(|part| part == b"D2R_HB") {
                                camera = Some(source);
                                let _ = registered_tx.try_send(source);
                                let response = p2p_packet(
                                    request.transmission_id,
                                    "<P2P><R2D_HB_R><rsp>0</rsp></R2D_HB_R></P2P>",
                                );
                                register.send_to(&response, source).unwrap();
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(error) => panic!("battery wake register failed: {error}"),
                    }
                    if matches!(wake_rx.try_recv(), Ok(()))
                        && let Some(camera) = camera
                    {
                        let wake = p2p_packet(3, "<P2P><R2D_C><sid>1</sid></R2D_C></P2P>");
                        register.send_to(&wake, camera).unwrap();
                    }
                    thread::sleep(Duration::from_millis(1));
                }
            });
            Self {
                endpoint,
                registered,
                wake,
                stop,
                worker: Some(worker),
            }
        }

        const fn endpoint(&self) -> BatteryWakeEndpoint {
            self.endpoint
        }

        fn wait_for_registration(&self) -> SocketAddr {
            self.registered
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
        }

        fn wake(&self) {
            self.wake.send(()).unwrap();
        }
    }

    impl Drop for BatteryWakeMock {
        fn drop(&mut self) {
            let _ = self.stop.send(());
            if let Some(worker) = self.worker.take() {
                worker.join().unwrap();
            }
        }
    }

    fn p2p_packet(transmission_id: u32, xml: &str) -> Vec<u8> {
        BcUdpPacket::Discovery(UdpDiscovery {
            transmission_id,
            xml: xml.as_bytes().to_vec(),
        })
        .encode()
        .unwrap()
    }

    #[test]
    fn detects_h265_mp4_sources() {
        let fixture = write_fixture(Codec::H265);
        let source = VideoSource::from_mp4(fixture.path()).unwrap();

        assert_eq!(source.codec, Codec::H265);
        assert_eq!(source.width, 16);
        assert_eq!(source.height, 16);
        assert!(!source.frames.is_empty());
    }

    #[test]
    fn checked_in_mp4_fixtures_cover_the_codec_and_resolution_matrix() {
        for (file_name, expected_codec, expected_width, expected_height) in [
            ("cc-4k-640x360-h264.mp4", Codec::H264, 640, 360),
            ("cc-4k-640x360-h265.mp4", Codec::H265, 640, 360),
            ("cc-4k-3840x2160-h264.mp4", Codec::H264, 3840, 2160),
            ("cc-4k-3840x2160-h265.mp4", Codec::H265, 3840, 2160),
        ] {
            let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("testdata")
                .join(file_name);
            let source = VideoSource::from_mp4(&fixture_path).unwrap();

            assert_eq!(source.codec, expected_codec, "{file_name}");
            assert_eq!(source.width, expected_width, "{file_name}");
            assert_eq!(source.height, expected_height, "{file_name}");
            assert_eq!(source.fps, 15, "{file_name}");
            assert_eq!(source.frames.len(), 15, "{file_name}");
            assert!(
                source.frames.first().is_some_and(|frame| frame.is_keyframe),
                "{file_name} must begin with a keyframe"
            );
        }

        let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join("cc-4k-640x360-h264.mp4");
        let source = VideoSource::from_mp4(&fixture_path).unwrap();
        assert!(
            source.frames.first().is_some_and(|frame| frame
                .data
                .starts_with(&[0, 0, 0, 1, 0x67, 0x42, 0xc0, 0x1f])),
            "low H.264 fixture must begin with a constrained-baseline level 3.1 SPS"
        );
    }

    #[test]
    fn checked_in_mp4_fixtures_stream_through_both_backends() {
        if !slow_tests_enabled() {
            return;
        }
        for (file_name, expected_codec) in [
            ("cc-4k-640x360-h264.mp4", reo_proto::VideoCodec::H264),
            ("cc-4k-640x360-h265.mp4", reo_proto::VideoCodec::H265),
            ("cc-4k-3840x2160-h264.mp4", reo_proto::VideoCodec::H264),
            ("cc-4k-3840x2160-h265.mp4", reo_proto::VideoCodec::H265),
        ] {
            let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("testdata")
                .join(file_name);

            {
                let camera = TestCameraBuilder::rtsp(&fixture_path, &fixture_path)
                    .start()
                    .unwrap();
                assert!(
                    rtsp_profile_yields_rtp(camera.connection().main_stream_url(), Transport::Tcp),
                    "{file_name} Retina main stream did not deliver RTP"
                );
                assert!(
                    rtsp_profile_yields_rtp(camera.connection().sub_stream_url(), Transport::Tcp),
                    "{file_name} Retina sub stream did not deliver RTP"
                );
            }

            let camera = TestCameraBuilder::reo_proto(&fixture_path, &fixture_path)
                .isolated_reo_ports()
                .start()
                .unwrap();
            assert_reo_camera_delivers_main_and_sub_frames(&camera, expected_codec);
        }
    }

    #[test]
    fn rtsp_camera_advertises_main_and_sub_profiles_through_onvif() {
        let fixture = write_fixture(Codec::H264);
        let camera = TestCameraBuilder::rtsp(fixture.path(), fixture.path())
            .start()
            .unwrap();
        let response = onvif_request(
            camera.connection(),
            "<s:Envelope><s:Body><trt:GetProfiles/></s:Body></s:Envelope>",
        );

        assert!(response.contains("token=\"main\""));
        assert!(response.contains("token=\"sub\""));
        assert!(response.contains("<tt:Encoding>H264</tt:Encoding>"));
    }

    #[test]
    fn rtsp_camera_exposes_a_fake_built_in_web_ui() {
        let fixture = write_fixture(Codec::H264);
        let camera = TestCameraBuilder::rtsp(fixture.path(), fixture.path())
            .start()
            .unwrap();
        let connection = camera.connection();
        let http_port = connection.http_port().expect("RTSP camera has fake web UI");
        assert!(
            connection
                .toml_entry("rtsp")
                .contains(&format!("http_port = {http_port}"))
        );
        assert!(connection.toml_entry("rtsp").contains(&format!(
            "main_rtsp_url = \"{}\"",
            connection.main_stream_url()
        )));
        assert!(connection.toml_entry("rtsp").contains(&format!(
            "sub_rtsp_url = \"{}\"",
            connection.sub_stream_url()
        )));

        let response =
            camera_web_ui_request(SocketAddr::new(IpAddr::V4(connection.ip()), http_port));
        assert!(response.contains("Fake Retina Camera"));
    }

    #[test]
    fn onvif_client_discovers_both_rtsp_profiles() {
        let fixture = write_fixture(Codec::H264);
        let camera = TestCameraBuilder::rtsp(fixture.path(), fixture.path())
            .start()
            .unwrap();
        let connection = camera.connection();
        let device_url = Url::parse(&format!(
            "http://{}:{}/onvif/device_service",
            connection.ip(),
            connection.onvif_port(),
        ))
        .unwrap();
        let credentials = Some(Credentials {
            username: "test".to_owned(),
            password: "test".to_owned(),
        });
        let device_client = ClientBuilder::new(&device_url)
            .credentials(credentials.clone())
            .auth_type(AuthType::Any)
            .build();
        let services = devicemgmt::get_services(
            &device_client,
            &devicemgmt::GetServices {
                include_capability: false,
            },
        )
        .unwrap();
        let media_url = services
            .service
            .iter()
            .find(|service| service.namespace.contains("media/wsdl"))
            .map(|service| service.x_addr.clone())
            .unwrap();
        let media_url = Url::parse(&media_url).unwrap();
        let media_client = ClientBuilder::new(&media_url)
            .credentials(credentials)
            .auth_type(AuthType::Any)
            .build();
        let profiles =
            onvif_media::get_profiles(&media_client, &onvif_media::GetProfiles {}).unwrap();

        assert_eq!(profiles.profiles.len(), 2);
        let main = profiles
            .profiles
            .iter()
            .find(|profile| profile.token.0 == "main")
            .unwrap();
        let stream_uri = onvif_media::get_stream_uri(
            &media_client,
            &onvif_media::GetStreamUri {
                stream_setup: onvif_xsd::StreamSetup {
                    stream: onvif_xsd::StreamType::RtpUnicast,
                    transport: onvif_xsd::Transport {
                        protocol: onvif_xsd::TransportProtocol::Rtsp,
                        tunnel: vec![],
                    },
                },
                profile_token: onvif_xsd::ReferenceToken(main.token.0.clone()),
            },
        )
        .unwrap();

        assert_eq!(stream_uri.media_uri.uri, connection.main_stream_url());
    }

    #[test]
    fn rtsp_camera_streams_main_and_sub_profiles() {
        if !slow_tests_enabled() {
            return;
        }
        for codec in [Codec::H264, Codec::H265] {
            for transport in [Transport::Tcp, Transport::Udp] {
                let fixture = write_fixture(codec);
                let camera = TestCameraBuilder::rtsp(fixture.path(), fixture.path())
                    .transport(transport)
                    .start()
                    .unwrap();
                let config = camera.connection().toml_entry("rtsp");
                assert!(config.contains("backend = \"retina\""));
                assert!(config.contains(&format!("transport = \"{}\"", transport.config_name())));

                for stream_url in [
                    camera.connection().main_stream_url(),
                    camera.connection().sub_stream_url(),
                    camera.connection().main_stream_url(),
                ] {
                    assert!(rtsp_profile_yields_rtp(stream_url, transport));
                }
            }
        }
    }

    #[test]
    fn realtime_rtsp_profiles_use_independent_servers_and_config_identity() {
        let fixture = write_fixture(Codec::H264);
        let config_ip = Ipv4Addr::new(192, 0, 2, 41);
        let camera = TestCameraBuilder::rtsp(fixture.path(), fixture.path())
            .config_ip(config_ip)
            .realtime_start_at(Duration::ZERO)
            .start()
            .unwrap();
        let connection = camera.connection();
        let main_authority = connection
            .main_stream_url()
            .strip_prefix("rtsp://")
            .unwrap()
            .split_once('/')
            .unwrap()
            .0;
        let sub_authority = connection
            .sub_stream_url()
            .strip_prefix("rtsp://")
            .unwrap()
            .split_once('/')
            .unwrap()
            .0;

        assert_eq!(connection.ip(), config_ip);
        assert_eq!(connection.endpoint_ip(), Ipv4Addr::LOCALHOST);
        assert_ne!(main_authority, sub_authority);
        assert!(rtsp_profile_yields_rtp(
            connection.main_stream_url(),
            Transport::Tcp
        ));
        assert!(rtsp_profile_yields_rtp(
            connection.sub_stream_url(),
            Transport::Tcp
        ));
        let config = connection.toml_entry("camera-1");
        assert!(config.contains("ip = \"192.0.2.41\""));
        assert!(!config.contains("onvif_port"));
        assert!(!config.contains("http_port"));
    }

    #[test]
    fn reo_camera_delivers_main_and_sub_frames() {
        for (source_codec, expected_codec) in [
            (Codec::H264, reo_proto::VideoCodec::H264),
            (Codec::H265, reo_proto::VideoCodec::H265),
        ] {
            let fixture = write_fixture(source_codec);
            let camera = TestCameraBuilder::reo_proto(fixture.path(), fixture.path())
                .isolated_reo_ports()
                .start()
                .unwrap();
            let config = camera.connection().toml_entry("reo");
            assert!(config.contains("backend = \"reo-proto\""));
            assert!(config.contains("transport = \"tcp\""));
            assert_reo_camera_delivers_main_and_sub_frames(&camera, expected_codec);
        }
    }

    #[test]
    fn reo_camera_exposes_a_fake_reolink_motion_control_api() {
        let fixture = write_fixture(Codec::H264);
        let camera = TestCameraBuilder::reo_proto(fixture.path(), fixture.path())
            .isolated_reo_ports()
            .start()
            .unwrap();
        let connection = camera.connection();
        let http_port = connection
            .http_port()
            .expect("Reo-proto camera has HTTP control API");
        let config = connection.toml_entry("reo");
        assert!(config.contains(&format!("http_port = {http_port}")));
        let address = SocketAddr::new(IpAddr::V4(connection.ip()), http_port);
        assert!(camera_web_ui_request(address).contains("Fake Reolink Camera"));

        let login = reolink_api_request(
            address,
            "Login",
            r#"[{"cmd":"Login","action":0,"param":{"User":{"userName":"test","password":"test"}}}]"#,
        );
        assert!(login.contains("fake-reolink-token"));

        let enc = reolink_api_request(
            address,
            "GetEnc",
            r#"[{"cmd":"GetEnc","action":0,"param":{"channel":0}}]"#,
        );
        assert!(enc.contains("mainStream"));
        assert!(enc.contains("subStream"));
        assert!(enc.contains("\"width\":16"));

        let ability = reolink_api_request(
            address,
            "GetAbility",
            r#"[{"cmd":"GetAbility","action":0,"param":{"User":{"userName":"test"}}}]"#,
        );
        assert!(ability.contains("abilityChn"));
        assert!(ability.contains("\"alarm\""));

        let initial = reolink_api_request(
            address,
            "GetMdState",
            r#"[{"cmd":"GetMdState","action":0,"param":{"channel":0}}]"#,
        );
        assert!(initial.contains("\"state\":1"));

        let updated = reolink_api_request(
            address,
            "SetAlarm",
            r#"[{"cmd":"SetAlarm","action":0,"param":{"Alarm":{"channel":0,"type":"md","enable":0}}}]"#,
        );
        assert!(updated.contains("\"code\":0"));

        let disabled = reolink_api_request(
            address,
            "GetMdState",
            r#"[{"cmd":"GetMdState","action":0,"param":{"channel":0}}]"#,
        );
        assert!(disabled.contains("\"state\":0"));
    }

    #[test]
    fn reo_udp_camera_delivers_main_and_sub_frames() {
        for (source_codec, expected_codec) in [
            (Codec::H264, reo_proto::VideoCodec::H264),
            (Codec::H265, reo_proto::VideoCodec::H265),
        ] {
            let fixture = write_fixture(source_codec);
            let camera = TestCameraBuilder::reo_proto(fixture.path(), fixture.path())
                .isolated_reo_ports()
                .transport(Transport::Udp)
                .start()
                .unwrap();
            assert!(
                camera
                    .connection()
                    .toml_entry("udp")
                    .contains("transport = \"udp\"")
            );
            assert!(
                camera
                    .connection()
                    .toml_entry("udp")
                    .contains("uid = \"TESTCAMERA0001\"")
            );

            let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_millis(100)))
                .unwrap();
            let local_port = socket.local_addr().unwrap().port();
            let now = Instant::now();
            let mut discovery = BcUdpDiscovery::new(
                BcUdpDiscoveryConfig {
                    transmission_id: 7,
                    ..BcUdpDiscoveryConfig::new("TESTCAMERA0001", 91, local_port)
                },
                now,
            )
            .unwrap();
            let BcUdpDiscoveryOutput::Datagram(request) = discovery.poll_output(now) else {
                panic!("expected initial Baichuan UDP discovery packet");
            };
            let server = SocketAddr::new(
                IpAddr::V4(camera.connection().ip()),
                camera.connection().bcudp_port().unwrap(),
            );
            socket.send_to(&request, server).unwrap();
            let mut datagram = [0_u8; 65_535];
            let (read, source) = socket.recv_from(&mut datagram).unwrap();
            discovery.handle_datagram(&datagram[..read]).unwrap();
            let BcUdpDiscoveryOutput::Connected(connection) = discovery.poll_output(Instant::now())
            else {
                panic!("expected completed Baichuan UDP discovery");
            };
            socket.connect(source).unwrap();
            let mut udp = connection
                .transport(Instant::now(), BcUdpConfig::default())
                .unwrap();
            let mut client = BcSession::default_client(Instant::now());
            client
                .handle_input(Input::Command(Command::Login(LoginParams::new(
                    "test",
                    "test",
                    EncryptionMode::BcEncrypt,
                ))))
                .unwrap();

            let deadline = Instant::now() + Duration::from_secs(2);
            let mut main_stream_id = None;
            let mut sub_stream_id = None;
            let mut saw_main = false;
            let mut saw_sub = false;
            let mut output = vec![0_u8; reo_proto::MAX_MEDIA_FRAME];

            while Instant::now() < deadline && !(saw_main && saw_sub) {
                drain_udp_client(
                    &mut client,
                    &mut udp,
                    &socket,
                    &mut output,
                    &mut main_stream_id,
                    &mut sub_stream_id,
                    &mut saw_main,
                    &mut saw_sub,
                    expected_codec,
                );
                match socket.recv(&mut datagram) {
                    Ok(read) => udp.handle_datagram(&datagram[..read]).unwrap(),
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                        ) =>
                    {
                        client.handle_input(Input::Timeout(Instant::now())).unwrap();
                    }
                    Err(error) => panic!("Baichuan UDP client failed: {error}"),
                }
                pump_udp(&mut udp, &socket, &mut client).unwrap();
            }

            assert!(saw_main, "main UDP stream never delivered a video frame");
            assert!(saw_sub, "sub UDP stream never delivered a video frame");
        }
    }

    #[test]
    fn battery_reo_camera_requires_p2p_wake_before_bcudp_discovery() {
        let fixture = write_fixture(Codec::H264);
        let wake = BatteryWakeMock::start();
        let camera =
            TestCameraBuilder::battery_reo_proto(fixture.path(), fixture.path(), wake.endpoint())
                .isolated_reo_ports()
                .transport(Transport::Udp)
                .start()
                .unwrap();
        assert!(camera.connection().is_battery());

        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let local_port = socket.local_addr().unwrap().port();
        let now = Instant::now();
        let mut discovery = BcUdpDiscovery::new(
            BcUdpDiscoveryConfig {
                transmission_id: 17,
                ..BcUdpDiscoveryConfig::new("TESTCAMERA0001", 91, local_port)
            },
            now,
        )
        .unwrap();
        let BcUdpDiscoveryOutput::Datagram(request) = discovery.poll_output(now) else {
            panic!("expected initial BCUDP discovery request");
        };
        let camera_address = SocketAddr::new(
            IpAddr::V4(camera.connection().ip()),
            camera.connection().bcudp_port().unwrap(),
        );
        socket.send_to(&request, camera_address).unwrap();
        let mut datagram = [0u8; 65_535];
        assert!(matches!(
            socket.recv_from(&mut datagram),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                )
        ));

        let registered = wake.wait_for_registration();
        assert_eq!(registered.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        wake.wake();

        let deadline = Instant::now() + Duration::from_secs(1);
        let mut connected = None;
        while Instant::now() < deadline {
            socket.send_to(&request, camera_address).unwrap();
            match socket.recv_from(&mut datagram) {
                Ok((read, source)) => {
                    discovery.handle_datagram(&datagram[..read]).unwrap();
                    if let BcUdpDiscoveryOutput::Connected(connection) =
                        discovery.poll_output(Instant::now())
                    {
                        connected = Some((connection, source));
                        break;
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) => {}
                Err(error) => panic!("battery BCUDP client failed: {error}"),
            }
        }
        assert!(
            connected.is_some(),
            "battery camera did not wake for BCUDP discovery"
        );
    }

    fn assert_reo_camera_delivers_main_and_sub_frames(
        camera: &TestCamera,
        expected_codec: reo_proto::VideoCodec,
    ) {
        let address = SocketAddr::new(
            IpAddr::V4(camera.connection().ip()),
            camera.connection().baichuan_port().unwrap(),
        );
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();

        let mut client = BcSession::default_client(Instant::now());
        client
            .handle_input(Input::Command(Command::Login(LoginParams::new(
                "test",
                "test",
                EncryptionMode::BcEncrypt,
            ))))
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut main_stream_id = None;
        let mut sub_stream_id = None;
        let mut saw_main = false;
        let mut saw_sub = false;
        let mut output = vec![0_u8; reo_proto::MAX_MEDIA_FRAME];
        let mut input = [0_u8; 64 * 1024];

        while Instant::now() < deadline && !(saw_main && saw_sub) {
            drain_client(
                &mut client,
                &mut stream,
                &mut output,
                &mut main_stream_id,
                &mut sub_stream_id,
                &mut saw_main,
                &mut saw_sub,
                expected_codec,
            );
            match stream.read(&mut input) {
                Ok(read) => client
                    .handle_input(Input::TcpData(Instant::now(), &input[..read]))
                    .unwrap(),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) =>
                {
                    client.handle_input(Input::Timeout(Instant::now())).unwrap();
                }
                Err(error) => panic!("Baichuan test connection failed: {error}"),
            }
        }

        assert!(saw_main, "main stream never delivered a video frame");
        assert!(saw_sub, "sub stream never delivered a video frame");
    }

    #[allow(clippy::too_many_arguments)]
    fn drain_client(
        client: &mut BcSession,
        stream: &mut TcpStream,
        output: &mut [u8],
        main_stream_id: &mut Option<u32>,
        sub_stream_id: &mut Option<u32>,
        saw_main: &mut bool,
        saw_sub: &mut bool,
        expected_codec: reo_proto::VideoCodec,
    ) {
        loop {
            match client.poll_output(output).unwrap() {
                Output::TcpSend { data } => stream.write_all(data).unwrap(),
                Output::Event(Event::LoggedIn(_)) => {
                    client
                        .handle_input(Input::Command(Command::SubscribeStream(
                            StreamSubscription {
                                channel: 0,
                                stream_type: StreamType::Main,
                                expected_width: 16,
                                expected_height: 16,
                            },
                        )))
                        .unwrap();
                    client
                        .handle_input(Input::Command(Command::SubscribeStream(
                            StreamSubscription {
                                channel: 0,
                                stream_type: StreamType::Sub,
                                expected_width: 16,
                                expected_height: 16,
                            },
                        )))
                        .unwrap();
                }
                Output::Event(Event::StreamSubscribed {
                    stream_id,
                    stream_type,
                    ..
                }) => match stream_type {
                    StreamType::Main => *main_stream_id = Some(stream_id),
                    StreamType::Sub => *sub_stream_id = Some(stream_id),
                    StreamType::Extern => {}
                },
                Output::Event(Event::VideoFrame {
                    stream_id,
                    codec,
                    data,
                    ..
                }) => {
                    assert_eq!(codec, expected_codec);
                    assert!(!data.is_empty(), "camera delivered an empty video frame");
                    if Some(stream_id) == *main_stream_id {
                        *saw_main = true;
                    }
                    if Some(stream_id) == *sub_stream_id {
                        *saw_sub = true;
                    }
                }
                Output::Event(_) | Output::Timeout(_) => return,
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn drain_udp_client(
        client: &mut BcSession,
        udp: &mut reo_proto::BcUdpTransport,
        socket: &UdpSocket,
        output: &mut [u8],
        main_stream_id: &mut Option<u32>,
        sub_stream_id: &mut Option<u32>,
        saw_main: &mut bool,
        saw_sub: &mut bool,
        expected_codec: reo_proto::VideoCodec,
    ) {
        loop {
            match client.poll_output(output).unwrap() {
                Output::TcpSend { data } => udp.queue_payload(data).unwrap(),
                Output::Event(Event::LoggedIn(_)) => {
                    client
                        .handle_input(Input::Command(Command::SubscribeStream(
                            StreamSubscription {
                                channel: 0,
                                stream_type: StreamType::Main,
                                expected_width: 16,
                                expected_height: 16,
                            },
                        )))
                        .unwrap();
                    client
                        .handle_input(Input::Command(Command::SubscribeStream(
                            StreamSubscription {
                                channel: 0,
                                stream_type: StreamType::Sub,
                                expected_width: 16,
                                expected_height: 16,
                            },
                        )))
                        .unwrap();
                }
                Output::Event(Event::StreamSubscribed {
                    stream_id,
                    stream_type,
                    ..
                }) => match stream_type {
                    StreamType::Main => *main_stream_id = Some(stream_id),
                    StreamType::Sub => *sub_stream_id = Some(stream_id),
                    StreamType::Extern => {}
                },
                Output::Event(Event::VideoFrame {
                    stream_id, codec, ..
                }) => {
                    assert_eq!(codec, expected_codec);
                    if Some(stream_id) == *main_stream_id {
                        *saw_main = true;
                    }
                    if Some(stream_id) == *sub_stream_id {
                        *saw_sub = true;
                    }
                }
                Output::Event(_) | Output::Timeout(_) => {
                    pump_udp(udp, socket, client).unwrap();
                    return;
                }
            }
        }
    }

    fn pump_udp(
        udp: &mut reo_proto::BcUdpTransport,
        socket: &UdpSocket,
        client: &mut BcSession,
    ) -> anyhow::Result<()> {
        loop {
            match udp.poll_output(Instant::now())? {
                BcUdpOutput::Datagram(datagram) => {
                    socket.send(&datagram)?;
                }
                BcUdpOutput::Payload(payload) => {
                    client.handle_input(Input::TcpData(Instant::now(), &payload))?;
                }
                BcUdpOutput::Timeout(_) => return Ok(()),
            }
        }
    }

    fn onvif_request(connection: &ConnectionInfo, body: &str) -> String {
        let address = SocketAddr::new(IpAddr::V4(connection.ip()), connection.onvif_port());
        let mut stream = TcpStream::connect(address).unwrap();
        let request = format!(
            "POST /onvif/media_service HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/soap+xml\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    fn reolink_api_request(address: SocketAddr, command: &str, body: &str) -> String {
        let mut stream = TcpStream::connect(address).unwrap();
        let request = format!(
            "POST /cgi-bin/api.cgi?cmd={command} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    fn camera_web_ui_request(address: SocketAddr) -> String {
        let mut stream = TcpStream::connect(address).unwrap();
        let request = format!("GET / HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    fn rtsp_profile_yields_rtp(stream_url: &str, transport: Transport) -> bool {
        let endpoint = stream_url.strip_prefix("rtsp://").unwrap();
        let (authority, _) = endpoint.split_once('/').unwrap();
        let mut stream = TcpStream::connect(authority).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut received = Vec::new();
        let udp = matches!(transport, Transport::Udp)
            .then(bind_udp_pair)
            .transpose()
            .unwrap();

        send_rtsp_request(
            &mut stream,
            &format!(
                "DESCRIBE {stream_url} RTSP/1.0\r\nCSeq: 1\r\nAccept: application/sdp\r\n\r\n"
            ),
        );
        assert!(take_rtsp_response(&mut stream, &mut received).contains("200 OK"));

        let track_url = format!("{stream_url}/trackID=0");
        let setup_transport = match &udp {
            Some((rtp, _)) => {
                let rtp_port = rtp.local_addr().unwrap().port();
                format!(
                    "RTP/AVP/UDP;unicast;client_port={rtp_port}-{}",
                    rtp_port + 1
                )
            }
            None => "RTP/AVP/TCP;unicast;interleaved=0-1".to_owned(),
        };
        send_rtsp_request(
            &mut stream,
            &format!(
                "SETUP {track_url} RTSP/1.0\r\nCSeq: 2\r\nTransport: {setup_transport}\r\n\r\n"
            ),
        );
        assert!(take_rtsp_response(&mut stream, &mut received).contains("200 OK"));

        send_rtsp_request(
            &mut stream,
            &format!("PLAY {stream_url} RTSP/1.0\r\nCSeq: 3\r\nSession: fake-session\r\n\r\n"),
        );
        assert!(take_rtsp_response(&mut stream, &mut received).contains("200 OK"));

        if let Some((rtp, _)) = udp {
            let mut packet = [0_u8; 4 * 1024];
            let read = rtp.recv(&mut packet).unwrap();
            return read >= 12 && packet[0] >> 6 == 2;
        }
        loop {
            if received.starts_with(b"$") {
                return received.len() >= 4;
            }
            let mut buffer = [0_u8; 4 * 1024];
            let read = stream.read(&mut buffer).unwrap();
            if read == 0 {
                return false;
            }
            received.extend_from_slice(&buffer[..read]);
        }
    }

    fn bind_udp_pair() -> std::io::Result<(UdpSocket, UdpSocket)> {
        for _ in 0..100 {
            let rtp = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
            let rtp_port = rtp.local_addr()?.port();
            let Some(rtcp_port) = rtp_port.checked_add(1) else {
                continue;
            };
            if let Ok(rtcp) = UdpSocket::bind((Ipv4Addr::LOCALHOST, rtcp_port)) {
                rtp.set_read_timeout(Some(Duration::from_secs(1)))?;
                return Ok((rtp, rtcp));
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AddrNotAvailable,
            "unable to allocate adjacent UDP RTP/RTCP ports",
        ))
    }

    fn send_rtsp_request(stream: &mut TcpStream, request: &str) {
        stream.write_all(request.as_bytes()).unwrap();
    }

    fn take_rtsp_response(stream: &mut TcpStream, received: &mut Vec<u8>) -> String {
        loop {
            if let Some(header_end) = received.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                let header_end = header_end + 4;
                let header = String::from_utf8_lossy(&received[..header_end]);
                let content_length = header
                    .lines()
                    .find_map(|line| line.strip_prefix("Content-Length: "))
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                let total = header_end + content_length;
                if received.len() >= total {
                    let response = String::from_utf8_lossy(&received[..total]).into_owned();
                    received.drain(..total);
                    return response;
                }
            }
            let mut buffer = [0_u8; 4 * 1024];
            let read = stream.read(&mut buffer).unwrap();
            assert!(
                read > 0,
                "RTSP server closed before completing its response"
            );
            received.extend_from_slice(&buffer[..read]);
        }
    }

    fn write_fixture(codec: Codec) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        let config = mp4::Mp4Config {
            major_brand: "isom".parse().unwrap(),
            minor_version: 512,
            compatible_brands: vec![
                "isom".parse().unwrap(),
                "iso2".parse().unwrap(),
                "avc1".parse().unwrap(),
                "mp41".parse().unwrap(),
            ],
            timescale: 90_000,
        };
        let mut writer = mp4::Mp4Writer::write_start(file.as_file_mut(), &config).unwrap();
        let media_conf = match codec {
            Codec::H264 => mp4::MediaConfig::AvcConfig(mp4::AvcConfig {
                width: 16,
                height: 16,
                seq_param_set: H264_SPS.to_vec(),
                pic_param_set: H264_PPS.to_vec(),
            }),
            Codec::H265 => mp4::MediaConfig::HevcConfig(mp4::HevcConfig {
                width: 16,
                height: 16,
                vps: vec![0x40, 0x01, 0x0c, 0x01],
                sps: vec![0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90],
                pps: vec![0x44, 0x01, 0xc0],
            }),
        };
        writer
            .add_track(&mp4::TrackConfig {
                track_type: mp4::TrackType::Video,
                timescale: 90_000,
                language: "und".to_owned(),
                media_conf,
            })
            .unwrap();
        for (index, sync) in [(0, true), (1, false)] {
            let payload = match codec {
                Codec::H264 => {
                    let nal = if sync {
                        [0x65, 0x88, 0x84, 0x21]
                    } else {
                        [0x41, 0x9a, 0x22, 0x11]
                    };
                    let mut sample = Vec::new();
                    sample.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                    sample.extend_from_slice(&nal);
                    sample
                }
                Codec::H265 => {
                    let nal = if sync {
                        [0x26, 0x01, 0x88, 0x84]
                    } else {
                        [0x02, 0x01, 0x9a, 0x22]
                    };
                    let mut sample = Vec::new();
                    sample.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                    sample.extend_from_slice(&nal);
                    sample
                }
            };
            writer
                .write_sample(
                    1,
                    &mp4::Mp4Sample {
                        start_time: index * 3_000,
                        duration: 3_000,
                        rendering_offset: 0,
                        is_sync: sync,
                        bytes: mp4::Bytes::from(payload),
                    },
                )
                .unwrap();
        }
        writer.write_end().unwrap();
        file.as_file_mut().sync_all().unwrap();
        file
    }
}
