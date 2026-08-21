use crate::{
    BatteryWakeEndpoint,
    media::{EncodedFrame, VideoSource},
};
use anyhow::{Context, anyhow};
use reo_proto::{
    CameraIdentity, PacketHeader,
    auth::{self, EncryptionMode},
    encryption,
    magic::{BC_CLASS_LEGACY, BC_CLASS_MODERN_EXT, HEADER_LEN_EXTENDED, make_status},
    stream::StreamType,
    {
        BcUdpConfig, BcUdpOutput, BcUdpPacket, BcUdpTransport, COMMAND_LOGIN, COMMAND_PING,
        COMMAND_PREVIEW_STOP, COMMAND_STREAM, COMMAND_TALK_CAPABILITIES, COMMAND_TALK_CONFIG,
        MAX_XML_BODY, UdpDiscovery,
    },
};
use std::{
    collections::{HashMap, VecDeque},
    io::{self, Read, Write},
    net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(10);
const READ_POLL_INTERVAL: Duration = Duration::from_millis(10);
const NONCE: &str = "TEST_CAMERA_NONCE";
const UDP_PRIMARY_PORT: u16 = 2018;
const UDP_SECONDARY_PORT: u16 = 2015;
const UDP_CAMERA_ID: i32 = 42;
const UDP_POLL_INTERVAL: Duration = Duration::from_millis(1);
const BATTERY_WAKE_RETRY_INTERVAL: Duration = Duration::from_millis(100);
const BATTERY_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(20);

pub struct ReoServer {
    stop: Option<Sender<()>>,
    worker: Option<JoinHandle<()>>,
    tcp_port: Option<u16>,
    _udp: ReoUdpServer,
    _battery_wake: Option<BatteryWakeClient>,
}

impl ReoServer {
    pub(crate) fn start(
        address: SocketAddr,
        username: String,
        password: String,
        main: VideoSource,
        sub: VideoSource,
        battery_wake: Option<BatteryWakeEndpoint>,
        uid: String,
    ) -> anyhow::Result<Self> {
        let awake = Arc::new(AtomicBool::new(battery_wake.is_none()));
        let udp_ports = if address.port() == 0 {
            [0, 0]
        } else {
            [UDP_PRIMARY_PORT, UDP_SECONDARY_PORT]
        };
        let udp = ReoUdpServer::start(
            address.ip(),
            udp_ports,
            username.clone(),
            password.clone(),
            main.clone(),
            sub.clone(),
            awake.clone(),
        )?;
        let battery_client = battery_wake
            .map(|endpoint| BatteryWakeClient::start(address.ip(), uid, endpoint, awake))
            .transpose()?;
        let (stop, worker, tcp_port) = if battery_client.is_some() {
            (None, None, None)
        } else {
            let listener = TcpListener::bind(address)
                .with_context(|| format!("unable to bind Baichuan listener on {address}"))?;
            let tcp_port = listener.local_addr()?.port();
            listener.set_nonblocking(true)?;
            let (stop, stopped) = mpsc::channel();
            let worker = thread::Builder::new()
                .name("test-camera-reo".to_owned())
                .spawn(move || serve(listener, stopped, username, password, main, sub))?;
            (Some(stop), Some(worker), Some(tcp_port))
        };
        Ok(Self {
            stop,
            worker,
            tcp_port,
            _udp: udp,
            _battery_wake: battery_client,
        })
    }

    pub(crate) const fn tcp_port(&self) -> Option<u16> {
        self.tcp_port
    }

    pub(crate) const fn primary_udp_port(&self) -> u16 {
        self._udp.primary_port
    }
}

impl Drop for ReoServer {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct ReoUdpServer {
    stop: Sender<()>,
    worker: Option<JoinHandle<()>>,
    primary_port: u16,
}

impl ReoUdpServer {
    fn start(
        bind_ip: IpAddr,
        ports: [u16; 2],
        username: String,
        password: String,
        main: VideoSource,
        sub: VideoSource,
        awake: Arc<AtomicBool>,
    ) -> anyhow::Result<Self> {
        let primary = UdpSocket::bind(SocketAddr::new(bind_ip, ports[0])).with_context(|| {
            format!(
                "unable to bind Baichuan UDP listener on {bind_ip}:{}",
                ports[0]
            )
        })?;
        let secondary = UdpSocket::bind(SocketAddr::new(bind_ip, ports[1])).with_context(|| {
            format!(
                "unable to bind Baichuan UDP listener on {bind_ip}:{}",
                ports[1]
            )
        })?;
        let primary_port = primary.local_addr()?.port();
        primary.set_nonblocking(true)?;
        secondary.set_nonblocking(true)?;
        let (stop, stopped) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("test-camera-reo-udp".to_owned())
            .spawn(move || {
                serve_udp(
                    [primary, secondary],
                    stopped,
                    username,
                    password,
                    main,
                    sub,
                    awake,
                );
            })?;
        Ok(Self {
            stop,
            worker: Some(worker),
            primary_port,
        })
    }
}

impl Drop for ReoUdpServer {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct UdpSession {
    socket_index: usize,
    peer: SocketAddr,
    transport: BcUdpTransport,
    camera: BaichuanCamera,
}

fn serve_udp(
    sockets: [UdpSocket; 2],
    stop: Receiver<()>,
    username: String,
    password: String,
    main: VideoSource,
    sub: VideoSource,
    awake: Arc<AtomicBool>,
) {
    let mut sessions = HashMap::new();
    let mut datagram = [0_u8; 65_535];
    loop {
        if stopped(&stop) {
            return;
        }
        for (socket_index, socket) in sockets.iter().enumerate() {
            loop {
                match socket.recv_from(&mut datagram) {
                    Ok((read, peer)) => {
                        if let Err(error) = handle_udp_datagram(
                            socket_index,
                            socket,
                            peer,
                            &datagram[..read],
                            &username,
                            &password,
                            &main,
                            &sub,
                            &awake,
                            &mut sessions,
                        ) {
                            tracing::debug!(%error, %peer, "invalid test Baichuan UDP datagram");
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) => {
                        tracing::warn!(%error, "test Baichuan UDP listener failed");
                        return;
                    }
                }
            }
        }

        let now = Instant::now();
        for session in sessions.values_mut() {
            if let Err(error) = service_udp_session(session, &sockets[session.socket_index], now) {
                tracing::debug!(%error, peer = %session.peer, "test Baichuan UDP session ended");
            }
        }
        thread::sleep(UDP_POLL_INTERVAL);
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_udp_datagram(
    socket_index: usize,
    socket: &UdpSocket,
    peer: SocketAddr,
    datagram: &[u8],
    username: &str,
    password: &str,
    main: &VideoSource,
    sub: &VideoSource,
    awake: &AtomicBool,
    sessions: &mut HashMap<(usize, SocketAddr), UdpSession>,
) -> anyhow::Result<()> {
    match BcUdpPacket::decode(datagram)? {
        BcUdpPacket::Discovery(discovery)
            if awake.load(Ordering::Acquire)
                && discovery.xml.windows(5).any(|part| part == b"C2D_C") =>
        {
            let client_id = xml_i32(&discovery.xml, "cid")
                .ok_or_else(|| anyhow!("Baichuan UDP discovery has no client id"))?;
            let response = BcUdpPacket::Discovery(UdpDiscovery {
                transmission_id: discovery.transmission_id,
                xml: connect_reply_xml(client_id, UDP_CAMERA_ID),
            })
            .encode()?;
            socket.send_to(&response, peer)?;
            sessions.insert(
                (socket_index, peer),
                UdpSession {
                    socket_index,
                    peer,
                    transport: BcUdpTransport::new(
                        UDP_CAMERA_ID,
                        client_id,
                        Instant::now(),
                        BcUdpConfig::default(),
                    )?,
                    camera: BaichuanCamera::new(username, password, main.clone(), sub.clone()),
                },
            );
        }
        _ => {
            if let Some(session) = sessions.get_mut(&(socket_index, peer)) {
                session.transport.handle_datagram(datagram)?;
            }
        }
    }
    Ok(())
}

struct BatteryWakeClient {
    stop: Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl BatteryWakeClient {
    fn start(
        bind_ip: IpAddr,
        uid: String,
        endpoint: BatteryWakeEndpoint,
        awake: Arc<AtomicBool>,
    ) -> anyhow::Result<Self> {
        let (stop, stopped) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("test-camera-battery-wake".to_owned())
            .spawn(move || serve_battery_wake(bind_ip, uid, endpoint, awake, stopped))?;
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
}

impl Drop for BatteryWakeClient {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn serve_battery_wake(
    bind_ip: IpAddr,
    uid: String,
    endpoint: BatteryWakeEndpoint,
    awake: Arc<AtomicBool>,
    stop: Receiver<()>,
) {
    let socket = match UdpSocket::bind(SocketAddr::new(bind_ip, 0)) {
        Ok(socket) => socket,
        Err(error) => {
            tracing::warn!(%error, "test battery camera failed to bind P2P socket");
            return;
        }
    };
    if let Err(error) = socket.set_read_timeout(Some(READ_POLL_INTERVAL)) {
        tracing::warn!(%error, "test battery camera failed to configure P2P socket");
        return;
    }

    let mut token = None;
    let mut next_query = Instant::now();
    let mut next_heartbeat = Instant::now();
    let mut datagram = [0u8; 4 * 1024];
    loop {
        if stopped(&stop) {
            return;
        }
        let now = Instant::now();
        if token.is_none() && now >= next_query {
            if let Err(error) = send_p2p_xml(
                &socket,
                endpoint.middleman(),
                1,
                format!("<P2P><D2M_Q><uid>{uid}</uid></D2M_Q></P2P>"),
            ) {
                tracing::debug!(%error, "test battery camera middleman query failed");
            }
            next_query = now + BATTERY_WAKE_RETRY_INTERVAL;
        }
        if let Some(token) = token
            && now >= next_heartbeat
        {
            if let Err(error) = send_p2p_xml(
                &socket,
                endpoint.register(),
                2,
                format!(
                    "<P2P><D2R_HB><uid>{uid}</uid><token>{token}</token><needrsp>1</needrsp></D2R_HB></P2P>"
                ),
            ) {
                tracing::debug!(%error, "test battery camera heartbeat failed");
            }
            next_heartbeat = now + BATTERY_HEARTBEAT_INTERVAL;
        }

        match socket.recv_from(&mut datagram) {
            Ok((read, _)) => match BcUdpPacket::decode(&datagram[..read]) {
                Ok(BcUdpPacket::Discovery(discovery)) => {
                    if discovery.xml.windows(7).any(|part| part == b"M2D_Q_R") {
                        let Some(next_token) = xml_u64(&discovery.xml, "token") else {
                            continue;
                        };
                        token = Some(next_token);
                        if let Err(error) = send_p2p_xml(
                            &socket,
                            endpoint.register(),
                            discovery.transmission_id,
                            format!(
                                "<P2P><D2R_R><uid>{uid}</uid><token>{next_token}</token></D2R_R></P2P>"
                            ),
                        ) {
                            tracing::debug!(%error, "test battery camera registration failed");
                        }
                        next_heartbeat = Instant::now();
                    } else if discovery.xml.windows(5).any(|part| part == b"R2D_C") {
                        awake.store(true, Ordering::Release);
                    }
                }
                Ok(_) => {}
                Err(error) => tracing::debug!(%error, "invalid test battery P2P datagram"),
            },
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) => {}
            Err(error) => {
                tracing::warn!(%error, "test battery camera P2P receive failed");
                return;
            }
        }
    }
}

fn send_p2p_xml(
    socket: &UdpSocket,
    destination: SocketAddr,
    transmission_id: u32,
    xml: String,
) -> anyhow::Result<()> {
    let packet = BcUdpPacket::Discovery(UdpDiscovery {
        transmission_id,
        xml: xml.into_bytes(),
    })
    .encode()?;
    socket.send_to(&packet, destination)?;
    Ok(())
}

fn service_udp_session(
    session: &mut UdpSession,
    socket: &UdpSocket,
    now: Instant,
) -> anyhow::Result<()> {
    session.camera.tick(now)?;
    queue_udp_camera_outbox(session)?;
    loop {
        match session.transport.poll_output(now)? {
            BcUdpOutput::Datagram(datagram) => {
                socket.send_to(&datagram, session.peer)?;
            }
            BcUdpOutput::Payload(payload) => {
                session.camera.receive(&payload)?;
                queue_udp_camera_outbox(session)?;
            }
            BcUdpOutput::Timeout(_) => return Ok(()),
        }
    }
}

fn queue_udp_camera_outbox(session: &mut UdpSession) -> anyhow::Result<()> {
    for payload in session.camera.take_outbox() {
        session.transport.queue_payload(&payload)?;
    }
    Ok(())
}

fn xml_i32(xml: &[u8], name: &str) -> Option<i32> {
    xml_text(xml, name)?.parse().ok()
}

fn xml_u64(xml: &[u8], name: &str) -> Option<u64> {
    xml_text(xml, name)?.parse().ok()
}

fn xml_text(xml: &[u8], name: &str) -> Option<String> {
    let mut value = None;
    reo_proto::xml::parse_xml(xml, |element, text| {
        if element == name {
            value = Some(text.to_owned());
        }
    })
    .ok()?;
    value
}

fn connect_reply_xml(client_id: i32, camera_id: i32) -> Vec<u8> {
    format!(
        "<P2P><D2C_C_R><rsp>0</rsp><cid>{client_id}</cid><did>{camera_id}</did></D2C_C_R></P2P>"
    )
    .into_bytes()
}

fn serve(
    listener: TcpListener,
    stop: Receiver<()>,
    username: String,
    password: String,
    main: VideoSource,
    sub: VideoSource,
) {
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                if let Err(error) =
                    serve_connection(stream, &stop, &username, &password, &main, &sub)
                {
                    tracing::debug!(%error, "test Baichuan client session ended");
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                match stop.recv_timeout(ACCEPT_POLL_INTERVAL) {
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                    Err(RecvTimeoutError::Timeout) => {}
                }
            }
            Err(error) => {
                tracing::warn!(%error, "test Baichuan listener failed");
                return;
            }
        }
    }
}

fn serve_connection(
    mut stream: TcpStream,
    stop: &Receiver<()>,
    username: &str,
    password: &str,
    main: &VideoSource,
    sub: &VideoSource,
) -> anyhow::Result<()> {
    stream.set_read_timeout(Some(READ_POLL_INTERVAL))?;
    stream.set_write_timeout(Some(READ_POLL_INTERVAL))?;
    let mut camera = BaichuanCamera::new(username, password, main.clone(), sub.clone());
    let mut read_buf = [0_u8; 64 * 1024];

    loop {
        if stopped(stop) {
            return Ok(());
        }
        match stream.read(&mut read_buf) {
            Ok(0) => return Ok(()),
            Ok(read) => camera.receive(&read_buf[..read])?,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) => {}
            Err(error) => return Err(error.into()),
        }
        camera.tick(Instant::now())?;
        camera.flush(&mut stream)?;
    }
}

fn stopped(stop: &Receiver<()>) -> bool {
    matches!(
        stop.try_recv(),
        Ok(()) | Err(mpsc::TryRecvError::Disconnected)
    )
}

struct BaichuanCamera {
    username: String,
    password: String,
    authenticated: bool,
    input: Vec<u8>,
    outbox: VecDeque<Vec<u8>>,
    main: Option<ActiveStream>,
    sub: Option<ActiveStream>,
    main_source: VideoSource,
    sub_source: VideoSource,
}

impl BaichuanCamera {
    fn new(username: &str, password: &str, main: VideoSource, sub: VideoSource) -> Self {
        Self {
            username: username.to_owned(),
            password: password.to_owned(),
            authenticated: false,
            input: Vec::new(),
            outbox: VecDeque::new(),
            main: None,
            sub: None,
            main_source: main,
            sub_source: sub,
        }
    }

    fn receive(&mut self, data: &[u8]) -> anyhow::Result<()> {
        self.input.extend_from_slice(data);
        loop {
            let (header, header_len) = match PacketHeader::parse(&self.input) {
                Ok(parsed) => parsed,
                Err(reo_proto::BcError::Incomplete) => return Ok(()),
                Err(error) => return Err(anyhow!(error.to_string())),
            };
            let total_len = header_len
                .checked_add(header.body_len as usize)
                .ok_or_else(|| anyhow!("Baichuan message length overflow"))?;
            if self.input.len() < total_len {
                return Ok(());
            }
            let body = self.input[header_len..total_len].to_vec();
            self.input.drain(..total_len);
            self.handle_message(header, &body)?;
        }
    }

    fn handle_message(&mut self, header: PacketHeader, body: &[u8]) -> anyhow::Result<()> {
        match header.msg_id {
            COMMAND_LOGIN => self.handle_login(header, body),
            COMMAND_PING => self.queue_packet(
                COMMAND_PING,
                &[],
                make_status(BC_CLASS_MODERN_EXT, 200),
                header.encryption_offset,
                Some(0),
            ),
            COMMAND_STREAM if header.is_modern() && self.authenticated => {
                self.start_stream(header)
            }
            COMMAND_PREVIEW_STOP if header.is_modern() => {
                self.main = None;
                self.sub = None;
                Ok(())
            }
            COMMAND_TALK_CAPABILITIES if header.is_modern() => self.queue_modern_xml(
                COMMAND_TALK_CAPABILITIES,
                "<body><TalkAbility version=\"1.1\"><audioStreamMode>0</audioStreamMode><duplex>1</duplex><audioConfig><sampleRate>8000</sampleRate><samplePrecision>16</samplePrecision><lengthPerEncoder>320</lengthPerEncoder></audioConfig></TalkAbility></body>",
                0,
            ),
            COMMAND_TALK_CONFIG if header.is_modern() => {
                self.queue_modern_xml(COMMAND_TALK_CONFIG, "<body><TalkConfig/></body>", 0)
            }
            message_id if header.is_modern() => {
                self.queue_modern_xml(message_id, "<body></body>", header.encryption_offset)
            }
            _ => Ok(()),
        }
    }

    fn handle_login(&mut self, header: PacketHeader, body: &[u8]) -> anyhow::Result<()> {
        if header.is_binary() && body.is_empty() {
            let mut xml = [0_u8; MAX_XML_BODY];
            let len = auth::build_nonce_response(NONCE, EncryptionMode::None, &mut xml)?;
            encryption::bc_xor(&mut xml[..len], 0);
            return self.queue_packet(
                COMMAND_LOGIN,
                &xml[..len],
                make_status(BC_CLASS_LEGACY, 0xDD00),
                0,
                None,
            );
        }
        if !header.is_modern() {
            return Ok(());
        }
        let mut decrypted = body.to_vec();
        encryption::bc_xor(&mut decrypted, 0);
        let (user_hash, password_hash) = auth::parse_modern_login(&decrypted)?;
        if !auth::validate_credentials(
            NONCE,
            &self.username,
            &self.password,
            user_hash.as_str(),
            password_hash.as_str(),
        ) {
            let mut rejected =
                b"<body><LoginUser><result>error</result></LoginUser></body>".to_vec();
            encryption::bc_xor(&mut rejected, 0);
            return self.queue_packet(
                COMMAND_LOGIN,
                &rejected,
                make_status(BC_CLASS_MODERN_EXT, 0),
                0,
                Some(0),
            );
        }

        let mut identity = CameraIdentity::default();
        identity.model.push_str("RLC-Test");
        identity.serial.push_str("TESTCAMERA0001");
        identity.firmware.push_str("test-camera");
        identity.channel_count = 1;
        let mut xml = [0_u8; MAX_XML_BODY];
        let len = auth::build_login_confirmation(1, &identity, &mut xml)?;
        encryption::bc_xor(&mut xml[..len], 0);
        self.authenticated = true;
        self.queue_packet(
            COMMAND_LOGIN,
            &xml[..len],
            make_status(BC_CLASS_MODERN_EXT, 0),
            0,
            Some(0),
        )
    }

    fn start_stream(&mut self, header: PacketHeader) -> anyhow::Result<()> {
        let stream_type = StreamType::from_wire_id(((header.encryption_offset >> 8) & 0xFF) as u8)
            .ok_or_else(|| anyhow!("unknown Baichuan stream type"))?;
        let stream = match stream_type {
            StreamType::Main => {
                ActiveStream::new(self.main_source.clone(), header.encryption_offset, 0)
            }
            StreamType::Sub => {
                ActiveStream::new(self.sub_source.clone(), header.encryption_offset, 256)
            }
            StreamType::Extern => return Ok(()),
        };
        let body = format!(
            "<body><Preview version=\"1.1\"><channelId>0</channelId><handle>{}</handle><streamType>{}</streamType></Preview></body>",
            stream.handle,
            stream_type.as_str(),
        );
        self.queue_packet(
            COMMAND_STREAM,
            body.as_bytes(),
            make_status(BC_CLASS_MODERN_EXT, 0),
            header.encryption_offset,
            Some(0),
        )?;
        match stream_type {
            StreamType::Main => self.main = Some(stream),
            StreamType::Sub => self.sub = Some(stream),
            StreamType::Extern => {}
        }
        Ok(())
    }

    fn tick(&mut self, now: Instant) -> anyhow::Result<()> {
        let mut packets = Vec::new();
        if let Some(stream) = &mut self.main
            && let Some(packet) = stream.next_packet(now)
        {
            packets.push(packet);
        }
        if let Some(stream) = &mut self.sub
            && let Some(packet) = stream.next_packet(now)
        {
            packets.push(packet);
        }
        for (offset, packet) in packets {
            self.queue_packet(
                COMMAND_STREAM,
                &packet,
                make_status(BC_CLASS_LEGACY, 0),
                offset,
                None,
            )?;
        }
        Ok(())
    }

    fn queue_modern_xml(
        &mut self,
        message_id: u32,
        body: &str,
        encryption_offset: u32,
    ) -> anyhow::Result<()> {
        self.queue_packet(
            message_id,
            body.as_bytes(),
            make_status(BC_CLASS_MODERN_EXT, 0),
            encryption_offset,
            Some(0),
        )
    }

    fn queue_packet(
        &mut self,
        message_id: u32,
        body: &[u8],
        status_class: u32,
        encryption_offset: u32,
        extension: Option<u32>,
    ) -> anyhow::Result<()> {
        let body_len = u32::try_from(body.len()).context("Baichuan body is too large")?;
        let header = PacketHeader {
            msg_id: message_id,
            body_len,
            encryption_offset,
            status_class,
            extension,
        };
        let mut header_buf = [0_u8; HEADER_LEN_EXTENDED];
        let header_len = header.serialize(&mut header_buf);
        let mut packet = Vec::with_capacity(header_len + body.len());
        packet.extend_from_slice(&header_buf[..header_len]);
        packet.extend_from_slice(body);
        self.outbox.push_back(packet);
        Ok(())
    }

    fn flush(&mut self, stream: &mut TcpStream) -> anyhow::Result<()> {
        for packet in self.take_outbox() {
            stream.write_all(&packet)?;
        }
        stream.flush()?;
        Ok(())
    }

    fn take_outbox(&mut self) -> VecDeque<Vec<u8>> {
        std::mem::take(&mut self.outbox)
    }
}

struct ActiveStream {
    source: VideoSource,
    header_offset: u32,
    handle: u32,
    frame_index: usize,
    metadata_sent: bool,
    next_emit: Instant,
}

impl ActiveStream {
    fn new(source: VideoSource, header_offset: u32, handle: u32) -> Self {
        Self {
            source,
            header_offset,
            handle,
            frame_index: 0,
            metadata_sent: false,
            next_emit: Instant::now(),
        }
    }

    fn next_packet(&mut self, now: Instant) -> Option<(u32, Vec<u8>)> {
        if now < self.next_emit {
            return None;
        }
        let frame = self.source.frames.get(self.frame_index)?.clone();
        let microseconds = u32::try_from(
            (self.frame_index as u128 * self.source.frame_interval.as_micros()) % 1_000_000,
        )
        .expect("subsecond timestamp fits u32");
        let mut packet = if self.metadata_sent {
            Vec::new()
        } else {
            self.metadata_sent = true;
            stream_metadata(&self.source)
        };
        packet.extend(video_frame(&self.source, &frame, self.handle, microseconds));
        self.frame_index = (self.frame_index + 1) % self.source.frames.len();
        self.next_emit = now + self.source.frame_interval;
        Some((self.header_offset, packet))
    }
}

fn stream_metadata(source: &VideoSource) -> Vec<u8> {
    let mut data = Vec::with_capacity(32);
    data.extend_from_slice(&reo_proto::media::MEDIA_MAGIC_INFO_V1.to_le_bytes());
    data.extend_from_slice(&30_u32.to_le_bytes());
    data.extend_from_slice(&source.width.to_le_bytes());
    data.extend_from_slice(&source.height.to_le_bytes());
    data.push(0);
    data.push(source.fps);
    data.extend_from_slice(&[26, 1, 1, 0, 0, 0]);
    data.extend_from_slice(&[26, 1, 1, 0, 0, 0]);
    pad_to_eight(&mut data);
    data
}

fn video_frame(
    source: &VideoSource,
    frame: &EncodedFrame,
    handle: u32,
    microseconds: u32,
) -> Vec<u8> {
    let magic = if frame.is_keyframe {
        reo_proto::media::MEDIA_MAGIC_IFRAME_BASE
    } else {
        reo_proto::media::MEDIA_MAGIC_PFRAME_BASE
    };
    let mut data = Vec::with_capacity(frame.data.len() + 32);
    data.extend_from_slice(&magic.to_le_bytes());
    data.extend_from_slice(source.codec.reo_name());
    data.extend_from_slice(&(frame.data.len() as u32).to_le_bytes());
    data.extend_from_slice(&0_u32.to_le_bytes());
    data.extend_from_slice(&microseconds.to_le_bytes());
    data.extend_from_slice(&handle.to_le_bytes());
    data.extend_from_slice(&frame.data);
    pad_to_eight(&mut data);
    data
}

fn pad_to_eight(data: &mut Vec<u8>) {
    data.resize((data.len() + 7) & !7, 0);
}
