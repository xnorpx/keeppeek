use crate::{
    battery_wake::BatteryWakeHandle,
    cameras::{
        AudioEncoding, BAICHUAN_PORT, CameraTransport, SessionTimestampNormalizer, VideoEncoding,
        local_broadcasts,
    },
    keeppeek::{KeepPeekEvent, StreamKind, VideoMeta},
    shutdown::Shutdown,
    stats::{
        CameraReport, HealthRegistry, IngressSnapshot, IngressStats, REPORT_INTERVAL, audio_report,
        log_camera_report, video_report,
    },
    storage::{
        AudioCodec, AudioFrame, MediaFrame, RecordingFrame, RecordingStreamIdentity, StorageHandle,
        VideoCodec, VideoFrame, nal,
    },
    webrtc::{Publisher, Source},
};
use bytes::Bytes;
use reo_proto::{
    BcUdpConfig, BcUdpConnection, BcUdpDiscovery, BcUdpDiscoveryConfig, BcUdpDiscoveryOutput,
    BcUdpOutput, LoginParams,
    alarm::{AlarmCommand, AlarmEventData, AlertEvent},
    auth::EncryptionMode,
    media::{AudioCodec as BcAudioCodec, StreamMetadata, VideoCodec as BcVideoCodec},
    session::{BcSession, BcSessionConfig, Command, Event, Input, Output, Role},
    stream::{SnapshotRequest, StreamSubscription, StreamType},
    video_cfg::{VideoCommand, VideoEvent},
};
use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, UdpSocket},
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError},
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::storage::metadata::{EventSource, TimelineEvent, event_icon};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const RECONNECT_DELAY: Duration = Duration::from_secs(5);
const MEDIA_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const UDP_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);
const UDP_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
const UDP_RECEIVE_BUFFER_SIZE: usize = 4 * 1024 * 1024;
const UDP_POLL_INTERVAL: Duration = Duration::from_millis(1);

trait BaichuanTransport {
    fn receive(&mut self, deadline: Instant, buf: &mut [u8]) -> anyhow::Result<Option<usize>>;
    fn send(&mut self, data: &[u8]) -> anyhow::Result<()>;
    fn close(&mut self) -> anyhow::Result<()>;
}

struct TcpBaichuanTransport {
    socket: TcpStream,
}

impl TcpBaichuanTransport {
    fn connect(addr: SocketAddr) -> anyhow::Result<Self> {
        let socket = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;
        socket.set_nodelay(true)?;
        Ok(Self { socket })
    }
}

impl BaichuanTransport for TcpBaichuanTransport {
    fn receive(&mut self, deadline: Instant, buf: &mut [u8]) -> anyhow::Result<Option<usize>> {
        let timeout = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(250))
            .max(Duration::from_millis(1));
        self.socket.set_read_timeout(Some(timeout))?;
        match self.socket.read(buf) {
            Ok(0) => anyhow::bail!("TCP connection closed by camera"),
            Ok(read) => Ok(Some(read)),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn send(&mut self, data: &[u8]) -> anyhow::Result<()> {
        self.socket.write_all(data)?;
        self.socket.flush()?;
        Ok(())
    }

    fn close(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

struct UdpBaichuanTransport {
    commands: Sender<UdpCommand>,
    payloads: Receiver<Result<Vec<u8>, String>>,
    thread: Option<JoinHandle<()>>,
}

enum UdpCommand {
    Payload(Vec<u8>),
    Close,
}

impl UdpBaichuanTransport {
    fn connect(
        camera_ip: IpAddr,
        uid: &str,
        battery_wake: Option<&BatteryWakeHandle>,
        shutdown: &Shutdown,
    ) -> anyhow::Result<Self> {
        if shutdown.is_cancelled() {
            anyhow::bail!("BCUDP discovery cancelled for {camera_ip}");
        }
        let socket = UdpSocket::bind(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0))?;
        socket.set_broadcast(true)?;
        socket2::SockRef::from(&socket).set_recv_buffer_size(UDP_RECEIVE_BUFFER_SIZE)?;
        let local_port = socket.local_addr()?.port();
        let now = Instant::now();
        let client_id = rand::random();
        let transmission_id = rand::random();
        let config = BcUdpDiscoveryConfig {
            transmission_id,
            ..BcUdpDiscoveryConfig::new(uid, client_id, local_port)
        };
        if let Some(battery_wake) = battery_wake {
            match battery_wake.request_wake(&socket, camera_ip, uid, client_id, transmission_id) {
                Ok(true) => tracing::debug!(ip = %camera_ip, "battery camera wake accepted"),
                Ok(false) => {
                    tracing::debug!(ip = %camera_ip, "battery camera wake unavailable; continuing direct discovery");
                }
                Err(error) => {
                    tracing::warn!(ip = %camera_ip, %error, "battery camera wake failed; continuing direct discovery");
                }
            }
        }
        let mut discovery = BcUdpDiscovery::new(config, now)?;
        let discovery_deadline = now + UDP_DISCOVERY_TIMEOUT;
        let mut destinations = vec![
            SocketAddr::new(camera_ip, 2018),
            SocketAddr::new(camera_ip, 2015),
            SocketAddr::new(Ipv4Addr::BROADCAST.into(), 2018),
            SocketAddr::new(Ipv4Addr::BROADCAST.into(), 2015),
        ];
        for broadcast in local_broadcasts()? {
            destinations.push(SocketAddr::new(broadcast.into(), 2018));
            destinations.push(SocketAddr::new(broadcast.into(), 2015));
        }
        destinations.sort_unstable();
        destinations.dedup();
        let mut datagram = [0u8; 65_535];
        let (connection, camera_addr) = loop {
            if shutdown.is_cancelled() {
                anyhow::bail!("BCUDP discovery cancelled for {camera_ip}");
            }
            let now = Instant::now();
            if now >= discovery_deadline {
                anyhow::bail!("BCUDP discovery timed out for {camera_ip}");
            }
            let next_send = match discovery.poll_output(now) {
                BcUdpDiscoveryOutput::Datagram(packet) => {
                    for &destination in &destinations {
                        socket.send_to(&packet, destination)?;
                    }
                    now + Duration::from_millis(500)
                }
                BcUdpDiscoveryOutput::Connected(_) => {
                    anyhow::bail!("BCUDP discovery completed without a camera address")
                }
                BcUdpDiscoveryOutput::Timeout(deadline) => deadline,
            };
            let timeout = next_send
                .min(discovery_deadline)
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(250))
                .max(Duration::from_millis(1));
            socket.set_read_timeout(Some(timeout))?;
            match socket.recv_from(&mut datagram) {
                Ok((read, source)) => {
                    discovery.handle_datagram(&datagram[..read])?;
                    if let BcUdpDiscoveryOutput::Connected(connection) =
                        discovery.poll_output(Instant::now())
                    {
                        break (connection, source);
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) => {}
                Err(error) => return Err(error.into()),
            }
        };
        socket.connect(camera_addr)?;
        let now = Instant::now();
        let transport = connection.transport(now, BcUdpConfig::default())?;
        let (command_tx, command_rx) = mpsc::channel();
        let (payload_tx, payload_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name(format!("baichuan-udp-{camera_ip}"))
            .spawn(move || {
                if let Err(error) =
                    run_udp_pump(socket, connection, transport, command_rx, &payload_tx)
                {
                    let _ = payload_tx.send(Err(error.to_string()));
                }
            })?;
        Ok(Self {
            commands: command_tx,
            payloads: payload_rx,
            thread: Some(thread),
        })
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        let _ = self.commands.send(UdpCommand::Close);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            anyhow::bail!("BCUDP socket thread panicked");
        }
        Ok(())
    }
}

impl BaichuanTransport for UdpBaichuanTransport {
    fn receive(&mut self, deadline: Instant, buf: &mut [u8]) -> anyhow::Result<Option<usize>> {
        let timeout = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(250))
            .max(Duration::from_millis(1));
        match self.payloads.recv_timeout(timeout) {
            Ok(Ok(payload)) => {
                if payload.len() > buf.len() {
                    anyhow::bail!(
                        "BCUDP payload of {} bytes exceeds receive buffer of {} bytes",
                        payload.len(),
                        buf.len()
                    );
                }
                buf[..payload.len()].copy_from_slice(&payload);
                Ok(Some(payload.len()))
            }
            Ok(Err(error)) => anyhow::bail!(error),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => anyhow::bail!("BCUDP socket thread stopped"),
        }
    }

    fn send(&mut self, data: &[u8]) -> anyhow::Result<()> {
        self.commands
            .send(UdpCommand::Payload(data.to_vec()))
            .map_err(|_| anyhow::anyhow!("BCUDP socket thread stopped"))?;
        Ok(())
    }

    fn close(&mut self) -> anyhow::Result<()> {
        self.stop()
    }
}

impl Drop for UdpBaichuanTransport {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn run_udp_pump(
    socket: UdpSocket,
    connection: BcUdpConnection,
    mut transport: reo_proto::BcUdpTransport,
    commands: Receiver<UdpCommand>,
    payloads: &Sender<Result<Vec<u8>, String>>,
) -> anyhow::Result<()> {
    let mut next_heartbeat = Instant::now();
    let mut datagram = [0u8; 65_535];
    loop {
        loop {
            match commands.try_recv() {
                Ok(UdpCommand::Payload(payload)) => transport.queue_payload(&payload)?,
                Ok(UdpCommand::Close) | Err(TryRecvError::Disconnected) => {
                    socket.send(&connection.disconnect()?)?;
                    return Ok(());
                }
                Err(TryRecvError::Empty) => break,
            }
        }

        let now = Instant::now();
        if now >= next_heartbeat {
            socket.send(&connection.heartbeat()?)?;
            next_heartbeat = now + UDP_HEARTBEAT_INTERVAL;
        }

        let transport_deadline = loop {
            match transport.poll_output(now)? {
                BcUdpOutput::Datagram(datagram) => {
                    socket.send(&datagram)?;
                }
                BcUdpOutput::Payload(payload) => {
                    payloads
                        .send(Ok(payload))
                        .map_err(|_| anyhow::anyhow!("BCUDP payload receiver stopped"))?;
                }
                BcUdpOutput::Timeout(deadline) => break deadline,
            }
        };

        let timeout = transport_deadline
            .min(next_heartbeat)
            .saturating_duration_since(Instant::now())
            .min(UDP_POLL_INTERVAL)
            .max(Duration::from_millis(1));
        socket.set_read_timeout(Some(timeout))?;
        match socket.recv(&mut datagram) {
            Ok(read) => transport.handle_datagram(&datagram[..read])?,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
}

struct StreamEntry {
    stream_id: Option<u32>,
    kind: StreamKind,
    stats: IngressStats,
    prev: IngressSnapshot,
    meta: VideoMeta,
    timestamps: SessionTimestampNormalizer,
    media_deadline: Option<Instant>,
}

fn reset_stream_bindings(streams: &mut [StreamEntry]) {
    for entry in streams {
        entry.stream_id = None;
        entry.timestamps.begin_session();
        entry.media_deadline = None;
        entry.prev = entry.stats.snapshot();
    }
}

fn bind_stream_entry(
    streams: &mut Vec<StreamEntry>,
    stream_id: u32,
    kind: StreamKind,
    expected_meta: VideoMeta,
    now: Instant,
) -> &mut StreamEntry {
    if let Some(index) = streams.iter().position(|entry| entry.kind == kind) {
        let entry = &mut streams[index];
        if entry.stream_id != Some(stream_id) {
            entry.stream_id = Some(stream_id);
            entry.timestamps.begin_session();
            entry.stats.on_connect();
            entry.media_deadline = Some(now + MEDIA_IDLE_TIMEOUT);
        }
        return entry;
    }

    let mut stats = IngressStats::new();
    stats.on_connect();
    let prev = stats.snapshot();
    streams.push(StreamEntry {
        stream_id: Some(stream_id),
        kind,
        stats,
        prev,
        meta: expected_meta,
        timestamps: SessionTimestampNormalizer::new(),
        media_deadline: Some(now + MEDIA_IDLE_TIMEOUT),
    });
    streams
        .last_mut()
        .expect("stream entry was pushed immediately before borrowing it")
}

fn note_video_progress(entry: &mut StreamEntry, now: Instant) {
    entry.media_deadline = Some(now + MEDIA_IDLE_TIMEOUT);
}

fn merge_video_meta(meta: &mut VideoMeta, width: u32, height: u32, framerate: f64) {
    if width > 0 {
        meta.width = width;
    }
    if height > 0 {
        meta.height = height;
    }
    if framerate > 0.0 {
        meta.framerate = framerate;
    }
}

fn update_stream_info(entry: &mut StreamEntry, width: u32, height: u32, framerate: f64) {
    merge_video_meta(&mut entry.meta, width, height, framerate);
    entry.stats.set_stream_info(
        entry.meta.encoding.clone(),
        entry.meta.width,
        entry.meta.height,
        entry.meta.framerate,
    );
}

fn update_stream_info_by_kind(
    streams: &mut [StreamEntry],
    kind: StreamKind,
    width: u32,
    height: u32,
    framerate: f64,
) {
    if let Some(entry) = streams.iter_mut().find(|entry| entry.kind == kind) {
        update_stream_info(entry, width, height, framerate);
    }
}

fn infer_supported_fps(measured_fps: f64, candidates: &[f64]) -> Option<f64> {
    let expected_fps = candidates.iter().copied().min_by(|left, right| {
        (left - measured_fps)
            .abs()
            .total_cmp(&(right - measured_fps).abs())
    })?;
    let relative_error = (expected_fps - measured_fps).abs() / expected_fps;
    (relative_error <= 0.2).then_some(expected_fps)
}

fn next_media_deadline(streams: &[StreamEntry]) -> Option<Instant> {
    streams
        .iter()
        .filter_map(|entry| entry.media_deadline)
        .min()
}

fn expired_media_stream(streams: &[StreamEntry], now: Instant) -> Option<StreamKind> {
    streams
        .iter()
        .find(|entry| entry.media_deadline.is_some_and(|deadline| now >= deadline))
        .map(|entry| entry.kind)
}

pub(crate) struct ReolinkLoop {
    pub camera_ip: IpAddr,
    pub camera_name: Option<String>,
    pub camera_brand: Option<String>,
    pub camera_uid: Option<String>,
    pub username: String,
    pub password: String,
    pub transport: CameraTransport,
    pub channel: u8,
    pub enable_main: bool,
    pub enable_sub: bool,
    pub main_expected_width: u32,
    pub main_expected_height: u32,
    pub main_expected_fps: f64,
    pub sub_expected_width: u32,
    pub sub_expected_height: u32,
    pub sub_expected_fps: f64,
    pub record_generic_motion_events: bool,
    pub storage: Option<StorageHandle>,
    pub live: Option<Publisher>,
    pub health: HealthRegistry,
    pub tx: SyncSender<KeepPeekEvent>,
    pub shutdown: Shutdown,
    pub battery_wake: Option<BatteryWakeHandle>,
}

impl ReolinkLoop {
    pub fn run(self) {
        let mut streams: Vec<StreamEntry> = Vec::new();
        let mut active_motion = HashMap::new();

        while !self.shutdown.is_cancelled() {
            match self.run_session(&mut streams, &mut active_motion) {
                Ok(()) => break,
                Err(error) => {
                    end_active_motion_events(&self.tx, &mut active_motion, unix_time_ms());
                    tracing::warn!(
                        ip = %self.camera_ip,
                        error = %error,
                        delay_secs = RECONNECT_DELAY.as_secs(),
                        "reconnecting baichuan session",
                    );
                    self.report_session_error(&mut streams, &error);
                    if self.shutdown.wait_timeout(RECONNECT_DELAY) {
                        break;
                    }
                }
            }
        }

        end_active_motion_events(&self.tx, &mut active_motion, unix_time_ms());
    }

    fn enabled_streams(&self) -> [Option<StreamKind>; 2] {
        [
            self.enable_main.then_some(StreamKind::Main),
            self.enable_sub.then_some(StreamKind::Sub),
        ]
    }

    fn report_session_error(&self, streams: &mut [StreamEntry], error: &anyhow::Error) {
        let error = error.to_string();
        for kind in self.enabled_streams().into_iter().flatten() {
            if let Some(entry) = streams.iter_mut().find(|entry| entry.kind == kind) {
                entry.stats.on_error();
            }
            let _ = self.tx.send(KeepPeekEvent::StreamError {
                camera_ip: self.camera_ip,
                stream: kind,
                error: error.clone(),
            });
        }
    }

    fn storage_label(&self) -> String {
        self.camera_name
            .as_deref()
            .unwrap_or(&self.camera_ip.to_string())
            .to_owned()
    }

    fn run_session(
        &self,
        streams: &mut Vec<StreamEntry>,
        active_motion: &mut HashMap<String, String>,
    ) -> anyhow::Result<()> {
        reset_stream_bindings(streams);
        let camera_id = self.camera_ip.to_string();
        let storage_label = self.storage_label();
        let addr = SocketAddr::new(self.camera_ip, BAICHUAN_PORT);
        tracing::info!(
            ip = %self.camera_ip,
            sub_enabled = self.enable_sub,
            "baichuan connecting to {}:{}",
            self.camera_ip,
            BAICHUAN_PORT,
        );

        let mut wire: Box<dyn BaichuanTransport> = match self.transport {
            CameraTransport::Tcp => Box::new(TcpBaichuanTransport::connect(addr)?),
            CameraTransport::Udp => Box::new(UdpBaichuanTransport::connect(
                self.camera_ip,
                self.camera_uid
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("BCUDP requires the camera UID"))?,
                self.battery_wake.as_ref(),
                &self.shutdown,
            )?),
        };

        let now = Instant::now();
        let config = BcSessionConfig {
            role: Role::Client,
            keepalive_channel: self.channel,
            // Reconnect the transport on media silence instead of relogging in-band.
            relogin_interval: Duration::from_secs(86400),
            stream_watchdog_interval: MEDIA_IDLE_TIMEOUT,
            ..BcSessionConfig::default_client()
        };
        let mut session = BcSession::new(config, now);

        session.handle_input(Input::Command(Command::Login(LoginParams::new(
            &self.username,
            &self.password,
            EncryptionMode::BcEncrypt,
        ))))?;

        let mut out_buf = vec![0u8; reo_proto::MAX_MEDIA_FRAME];
        let mut deadline = drain_outputs_simple(&mut session, wire.as_mut(), &mut out_buf)?;

        let mut tcp_buf = vec![0u8; 64 * 1024];
        let mut main_meta = VideoMeta {
            encoding: VideoEncoding::H265,
            width: self.main_expected_width,
            height: self.main_expected_height,
            framerate: self.main_expected_fps,
        };
        let mut sub_meta = VideoMeta {
            encoding: VideoEncoding::H265,
            width: self.sub_expected_width,
            height: self.sub_expected_height,
            framerate: self.sub_expected_fps,
        };
        let mut pending_metadata = HashMap::<u32, StreamMetadata>::new();
        let mut main_framerates = Vec::new();
        let mut sub_framerates = Vec::new();
        let mut active_ids: Vec<u32> = Vec::new();
        let mut last_report = Instant::now();
        let mut audio_codec: Option<String> = None;
        let mut pending_snapshots = VecDeque::<Vec<String>>::new();
        let mut snapshot_in_flight = false;

        loop {
            if self.shutdown.is_cancelled() {
                tracing::info!(
                    ip = %self.camera_ip,
                    stats = ?session.stats(),
                    "shutting down baichuan session",
                );
                for &id in &active_ids {
                    let _ = session
                        .handle_input(Input::Command(Command::UnsubscribeStream { stream_id: id }));
                }
                let _ = drain_outputs_simple(&mut session, wire.as_mut(), &mut out_buf);
                let _ = wire.close();
                return Ok(());
            }

            let receive_deadline = next_media_deadline(streams)
                .map_or(deadline, |media_deadline| deadline.min(media_deadline));
            if let Some(read) = wire.receive(receive_deadline, &mut tcp_buf)? {
                let now = Instant::now();
                match session.handle_input(Input::TcpData(now, &tcp_buf[..read])) {
                    Ok(()) => {}
                    Err(reo_proto::BcError::XmlParse(msg)) => {
                        tracing::warn!(ip = %self.camera_ip, msg, "ignoring XML parse error");
                    }
                    Err(error) => return Err(error.into()),
                }
            }

            let now = Instant::now();
            session.handle_input(Input::Timeout(now))?;

            loop {
                match session.poll_output(&mut out_buf) {
                    Ok(Output::TcpSend { data }) => {
                        wire.send(data)?;
                    }
                    Ok(Output::Event(event)) => match event {
                        Event::LoggedIn(result) => {
                            tracing::info!(
                                ip = %self.camera_ip,
                                model = %result.camera_identity.model,
                                firmware = %result.camera_identity.firmware,
                                sub_enabled = self.enable_sub,
                                "baichuan logged in",
                            );

                            if self.enable_main {
                                session.handle_input(Input::Command(Command::SubscribeStream(
                                    StreamSubscription {
                                        channel: self.channel,
                                        stream_type: StreamType::Main,
                                        expected_width: self.main_expected_width,
                                        expected_height: self.main_expected_height,
                                    },
                                )))?;
                            }

                            if self.enable_sub {
                                session.handle_input(Input::Command(Command::SubscribeStream(
                                    StreamSubscription {
                                        channel: self.channel,
                                        stream_type: StreamType::Sub,
                                        expected_width: self.sub_expected_width,
                                        expected_height: self.sub_expected_height,
                                    },
                                )))?;
                            }

                            session.handle_input(Input::Command(Command::Video(
                                VideoCommand::GetStreamCatalog,
                            )))?;
                            session.handle_input(Input::Command(Command::Video(
                                VideoCommand::GetCompression {
                                    channel: self.channel,
                                },
                            )))?;

                            if let Err(error) = session.handle_input(Input::Command(
                                Command::Alarm(AlarmCommand::StartMotionAlarm {
                                    channel: self.channel,
                                }),
                            )) {
                                tracing::warn!(ip = %self.camera_ip, %error, "camera motion events are unavailable");
                            }
                        }
                        Event::StreamSubscribed {
                            stream_id,
                            channel,
                            stream_type,
                        } => {
                            let kind = match stream_type {
                                StreamType::Main => StreamKind::Main,
                                StreamType::Sub => StreamKind::Sub,
                                StreamType::Extern => continue,
                            };
                            if channel != self.channel {
                                continue;
                            }

                            if !active_ids.contains(&stream_id) {
                                active_ids.push(stream_id);
                            }

                            let expected_meta = match kind {
                                StreamKind::Main => main_meta.clone(),
                                StreamKind::Sub => sub_meta.clone(),
                            };
                            let entry = bind_stream_entry(
                                streams,
                                stream_id,
                                kind,
                                expected_meta,
                                Instant::now(),
                            );
                            if let Some(info) = pending_metadata.remove(&stream_id) {
                                update_stream_info(entry, info.width, info.height, info.fps as f64);
                            } else {
                                update_stream_info(entry, 0, 0, 0.0);
                            }

                            let _ = self.tx.send(KeepPeekEvent::StreamConnected {
                                camera_ip: self.camera_ip,
                                stream: kind,
                            });

                            tracing::info!(
                                ip = %self.camera_ip,
                                channel,
                                stream_id,
                                stream = %kind,
                                "subscribed stream",
                            );
                        }
                        Event::LoginFailed(status) => {
                            return Err(anyhow::anyhow!(
                                "baichuan login failed (status 0x{status:X})"
                            ));
                        }
                        Event::StreamMetadata { stream_id, info } => {
                            if let Some(entry) = streams
                                .iter_mut()
                                .find(|entry| entry.stream_id == Some(stream_id))
                            {
                                update_stream_info(entry, info.width, info.height, info.fps as f64);
                            } else {
                                pending_metadata.insert(stream_id, info);
                            }
                        }
                        Event::Video(VideoEvent::StreamCatalog(info)) => {
                            merge_video_meta(
                                &mut main_meta,
                                info.main_width,
                                info.main_height,
                                0.0,
                            );
                            merge_video_meta(&mut sub_meta, info.sub_width, info.sub_height, 0.0);
                            update_stream_info_by_kind(
                                streams,
                                StreamKind::Main,
                                info.main_width,
                                info.main_height,
                                0.0,
                            );
                            update_stream_info_by_kind(
                                streams,
                                StreamKind::Sub,
                                info.sub_width,
                                info.sub_height,
                                0.0,
                            );
                            main_framerates = info
                                .main_framerates
                                .iter()
                                .copied()
                                .map(f64::from)
                                .collect();
                            sub_framerates =
                                info.sub_framerates.iter().copied().map(f64::from).collect();
                            if main_framerates.is_empty() && info.main_default_fps > 0 {
                                main_framerates.push(info.main_default_fps as f64);
                            }
                            if sub_framerates.is_empty() && info.sub_default_fps > 0 {
                                sub_framerates.push(info.sub_default_fps as f64);
                            }
                        }
                        Event::Video(VideoEvent::Compression(profiles)) => {
                            for (kind, info) in [
                                (StreamKind::Main, profiles.main),
                                (StreamKind::Sub, profiles.sub),
                            ] {
                                let Some(info) = info else {
                                    continue;
                                };
                                let meta = match kind {
                                    StreamKind::Main => &mut main_meta,
                                    StreamKind::Sub => &mut sub_meta,
                                };
                                merge_video_meta(
                                    meta,
                                    info.resolution_width,
                                    info.resolution_height,
                                    info.fps as f64,
                                );
                                update_stream_info_by_kind(
                                    streams,
                                    kind,
                                    info.resolution_width,
                                    info.resolution_height,
                                    info.fps as f64,
                                );
                            }
                        }
                        Event::VideoFrame {
                            stream_id,
                            codec,
                            is_keyframe,
                            data,
                            timestamp,
                            ..
                        } => {
                            let (encoding, frame_codec) = match codec {
                                BcVideoCodec::H264 => (VideoEncoding::H264, VideoCodec::H264),
                                BcVideoCodec::H265 => (VideoEncoding::H265, VideoCodec::H265),
                            };

                            let Some(entry) = streams
                                .iter_mut()
                                .find(|entry| entry.stream_id == Some(stream_id))
                            else {
                                tracing::debug!(
                                    ip = %self.camera_ip,
                                    stream_id,
                                    "dropping video frame with unknown stream id",
                                );
                                continue;
                            };

                            let received_at = Instant::now();
                            note_video_progress(entry, received_at);
                            let timestamp = entry.timestamps.normalize(timestamp);
                            entry.meta.encoding = encoding.clone();
                            entry.stats.set_stream_info(
                                encoding,
                                entry.meta.width,
                                entry.meta.height,
                                entry.meta.framerate,
                            );
                            entry.stats.on_video_frame(is_keyframe, data.len());

                            let avcc = Bytes::from(nal::annexb_to_avcc(data));
                            if let Some(live) = &self.live {
                                live.publish(
                                    Source {
                                        camera_ip: self.camera_ip,
                                        stream: entry.kind,
                                    },
                                    frame_codec,
                                    is_keyframe,
                                    received_at,
                                    Some(timestamp),
                                    avcc.clone(),
                                );
                            }
                            let frame = MediaFrame::Video(VideoFrame {
                                codec: frame_codec,
                                is_keyframe,
                                width: entry.meta.width,
                                height: entry.meta.height,
                                data: avcc,
                            });
                            if let Some(ref storage) = self.storage {
                                storage.ingest_stream(
                                    RecordingStreamIdentity::new(
                                        camera_id.clone(),
                                        entry.kind.to_string(),
                                        &storage_label,
                                    ),
                                    RecordingFrame {
                                        received_at,
                                        timestamp: Some(timestamp),
                                        frame,
                                    },
                                );
                            }
                        }
                        Event::AudioFrame {
                            stream_id,
                            codec,
                            data,
                            duration,
                        } => {
                            let preferred_kind = self
                                .storage
                                .as_ref()
                                .map(|storage| storage.preferred_audio_stream(&camera_id))
                                .and_then(|stream| match stream {
                                    "main" => Some(StreamKind::Main),
                                    "sub" => Some(StreamKind::Sub),
                                    _ => None,
                                })
                                .filter(|kind| streams.iter().any(|entry| entry.kind == *kind))
                                .unwrap_or({
                                    if self.enable_main {
                                        StreamKind::Main
                                    } else {
                                        StreamKind::Sub
                                    }
                                });
                            let Some(audio_stream) = streams
                                .iter()
                                .find(|entry| entry.stream_id == Some(stream_id))
                                .map(|entry| entry.kind)
                            else {
                                continue;
                            };
                            if audio_stream != preferred_kind {
                                continue;
                            }
                            let (encoding, frame_codec) = match codec {
                                BcAudioCodec::Aac => (AudioEncoding::AAC, Some(AudioCodec::Aac)),
                                BcAudioCodec::G711Alaw => {
                                    (AudioEncoding::G711, Some(AudioCodec::G711Alaw))
                                }
                                BcAudioCodec::G711Ulaw => {
                                    (AudioEncoding::G711, Some(AudioCodec::G711Ulaw))
                                }
                                other => (AudioEncoding::Unknown(format!("{other:?}")), None),
                            };
                            if audio_codec.is_none() {
                                tracing::info!(
                                    ip = %self.camera_ip,
                                    codec = %encoding,
                                    "first audio frame received"
                                );
                            }
                            audio_codec = Some(encoding.to_string());

                            if let Some(entry) =
                                streams.iter_mut().find(|entry| entry.kind == audio_stream)
                            {
                                entry.stats.on_audio_frame(data.len());
                            }
                            if let Some(fc) = frame_codec {
                                let sample_rate = fc.default_sample_rate();
                                if let Some(storage) = &self.storage {
                                    let frame = MediaFrame::Audio(AudioFrame {
                                        codec: fc,
                                        sample_rate,
                                        duration,
                                        data: data.to_vec(),
                                    });
                                    storage.ingest_stream(
                                        RecordingStreamIdentity::new(
                                            camera_id.clone(),
                                            audio_stream.to_string(),
                                            &storage_label,
                                        ),
                                        RecordingFrame {
                                            received_at: Instant::now(),
                                            timestamp: None,
                                            frame,
                                        },
                                    );
                                }
                            }
                        }
                        Event::Alarm(AlertEvent::MotionAlarmStarted) => {
                            tracing::debug!(ip = %self.camera_ip, "camera motion event subscription active");
                        }
                        Event::Alarm(AlertEvent::AlarmEventList(events)) => {
                            let mut started_event_ids = Vec::new();
                            let mut received_alarm = false;
                            let mut received_active_alarm = false;

                            for data in events.events {
                                if data.channel != self.channel {
                                    continue;
                                }
                                received_alarm = true;
                                for kind in
                                    alarm_event_kinds(&data, self.record_generic_motion_events)
                                {
                                    received_active_alarm = true;
                                    if active_motion.contains_key(&kind) {
                                        continue;
                                    }
                                    let event_id = random_event_id();
                                    active_motion.insert(kind.clone(), event_id.clone());
                                    let icon_key = event_icon(None, &kind).key.to_owned();
                                    let event = TimelineEvent {
                                        id: event_id.clone(),
                                        revision: 1,
                                        camera_id: self.camera_ip.to_string(),
                                        stream: None,
                                        source: EventSource::Camera,
                                        kind,
                                        start_time_ms: unix_time_ms(),
                                        end_time_ms: None,
                                        confidence: None,
                                        bbox: None,
                                        bbox_attachment_id: None,
                                        zone: None,
                                        attachments: Vec::new(),
                                        canonical_attachment_id: None,
                                        icon_key,
                                        rejected_icon_key: None,
                                        thumbnail_filename: None,
                                    };
                                    let _ = self.tx.send(KeepPeekEvent::TimelineEventStarted {
                                        event: Box::new(event),
                                    });
                                    started_event_ids.push(event_id);
                                }
                            }

                            if received_alarm && !received_active_alarm {
                                let ended_at_ms = unix_time_ms();
                                for event_id in active_motion.drain().map(|(_, event_id)| event_id)
                                {
                                    let _ = self.tx.send(KeepPeekEvent::TimelineEventEnded {
                                        id: event_id,
                                        end_time_ms: ended_at_ms,
                                    });
                                }
                            }

                            if !started_event_ids.is_empty() {
                                pending_snapshots.push_back(started_event_ids);
                                if let Err(error) = request_next_snapshot(
                                    &mut session,
                                    self.channel,
                                    &pending_snapshots,
                                    &mut snapshot_in_flight,
                                ) {
                                    let _ = pending_snapshots.pop_front();
                                    tracing::warn!(ip = %self.camera_ip, %error, "unable to request event snapshot");
                                }
                            }
                        }
                        Event::SnapshotData { data } => {
                            snapshot_in_flight = false;
                            if let Some(event_ids) = pending_snapshots.pop_front() {
                                for event_id in event_ids {
                                    let _ = self.tx.send(KeepPeekEvent::TimelineEventThumbnail {
                                        camera_id: self.camera_ip.to_string(),
                                        event_id,
                                        jpeg: data.to_vec(),
                                    });
                                }
                            }
                            if let Err(error) = request_next_snapshot(
                                &mut session,
                                self.channel,
                                &pending_snapshots,
                                &mut snapshot_in_flight,
                            ) {
                                let _ = pending_snapshots.pop_front();
                                tracing::warn!(ip = %self.camera_ip, %error, "unable to request event snapshot");
                            }
                        }
                        Event::SnapshotFailed { status } => {
                            snapshot_in_flight = false;
                            let event_count =
                                pending_snapshots.pop_front().map_or(0, |ids| ids.len());
                            tracing::warn!(
                                ip = %self.camera_ip,
                                status,
                                event_count,
                                "camera rejected event snapshot",
                            );
                            if let Err(error) = request_next_snapshot(
                                &mut session,
                                self.channel,
                                &pending_snapshots,
                                &mut snapshot_in_flight,
                            ) {
                                let _ = pending_snapshots.pop_front();
                                tracing::warn!(ip = %self.camera_ip, %error, "unable to request event snapshot");
                            }
                        }
                        Event::CommandFailed {
                            msg_id,
                            msg_num,
                            status,
                        } if msg_id == reo_proto::COMMAND_STREAM => {
                            anyhow::bail!(
                                "camera rejected stream request {msg_num} with status {status}"
                            );
                        }
                        Event::CommandFailed {
                            msg_id,
                            msg_num,
                            status,
                        } if msg_id == reo_proto::COMMAND_START_MOTION_ALARM => {
                            tracing::warn!(
                                ip = %self.camera_ip,
                                msg_num,
                                status,
                                "camera rejected motion event subscription",
                            );
                        }
                        Event::CommandFailed {
                            msg_id,
                            msg_num,
                            status,
                        } if msg_id == reo_proto::COMMAND_PING => {
                            anyhow::bail!(
                                "camera rejected keepalive request {msg_num} with status {status}"
                            );
                        }
                        Event::SessionTimeout => {
                            anyhow::bail!(
                                "baichuan media stream made no progress before its deadline"
                            );
                        }
                        Event::Pong => {}
                        _ => {}
                    },
                    Ok(Output::Timeout(dl)) => {
                        deadline = dl;
                        break;
                    }
                    Err(reo_proto::BcError::BufferTooSmall { needed, .. }) => {
                        out_buf.resize(needed, 0);
                    }
                    Err(reo_proto::BcError::XmlParse(_)) => {}
                    Err(e) => return Err(e.into()),
                }
            }

            if let Some(kind) = expired_media_stream(streams, Instant::now()) {
                anyhow::bail!(
                    "baichuan {kind} video stream made no media progress before its deadline"
                );
            }

            if last_report.elapsed() >= REPORT_INTERVAL && !streams.is_empty() {
                last_report = Instant::now();
                tracing::debug!(ip = %self.camera_ip, stats = ?session.stats(), "baichuan session stats");
                let mut stream_reports = Vec::new();
                for entry in streams.iter_mut() {
                    let snap = entry.stats.snapshot();
                    let rates = snap.rates_since(&entry.prev);
                    if entry.meta.framerate <= 0.0 {
                        let candidates = match entry.kind {
                            StreamKind::Main => &main_framerates,
                            StreamKind::Sub => &sub_framerates,
                        };
                        if let Some(expected_fps) = infer_supported_fps(rates.video_fps, candidates)
                        {
                            update_stream_info(entry, 0, 0, expected_fps);
                        }
                    }
                    stream_reports.push(video_report(
                        entry.kind,
                        &snap,
                        &rates,
                        entry.stats.codec.as_ref(),
                        entry.stats.width,
                        entry.stats.height,
                        entry.stats.expected_fps,
                    ));
                    if entry.kind == StreamKind::Main
                        && let Some(audio) = audio_report(&snap, &rates, audio_codec.as_deref())
                    {
                        stream_reports.push(audio);
                    }
                    entry.prev = snap;
                }
                let report = CameraReport {
                    ip: self.camera_ip,
                    name: self.camera_name.clone(),
                    brand: self.camera_brand.clone(),
                    port: BAICHUAN_PORT,
                    streams: stream_reports,
                };
                log_camera_report(&report);
                self.health.publish(report);
            }
        }
    }
}

fn normalize_alarm_kind(kind: &str) -> String {
    let normalized = kind.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "" | "1" | "true" | "md" | "motion" => "motion".to_owned(),
        "people" | "person" => "person".to_owned(),
        "dog_cat" | "dogcat" | "animal" => "animal".to_owned(),
        "car" | "vehicle" => "vehicle".to_owned(),
        other => other.to_owned(),
    }
}

fn alarm_event_kinds(data: &AlarmEventData, record_generic_motion_events: bool) -> Vec<String> {
    if !data.is_active() {
        return Vec::new();
    }
    let legacy_alarm_type = if data.status.is_empty() {
        data.alarm_type.as_str()
    } else {
        ""
    };
    let mut kinds = Vec::with_capacity(3);
    for value in [
        data.status.as_str(),
        legacy_alarm_type,
        data.ai_types.as_str(),
    ] {
        for kind in value.split(',').map(str::trim) {
            if kind.is_empty()
                || kind.eq_ignore_ascii_case("none")
                || kind == "0"
                || kind.eq_ignore_ascii_case("false")
            {
                continue;
            }
            let kind = normalize_alarm_kind(kind);
            if (record_generic_motion_events || kind != "motion")
                && matches!(kind.as_str(), "motion" | "person" | "animal" | "vehicle")
                && !kinds.contains(&kind)
            {
                kinds.push(kind);
            }
        }
    }
    kinds
}

fn end_active_motion_events(
    tx: &SyncSender<KeepPeekEvent>,
    active_motion: &mut HashMap<String, String>,
    end_time_ms: i64,
) {
    for event_id in active_motion.drain().map(|(_, event_id)| event_id) {
        let _ = tx.send(KeepPeekEvent::TimelineEventEnded {
            id: event_id,
            end_time_ms,
        });
    }
}

fn request_next_snapshot(
    session: &mut BcSession,
    channel: u8,
    pending_snapshots: &VecDeque<Vec<String>>,
    snapshot_in_flight: &mut bool,
) -> Result<(), reo_proto::BcError> {
    if *snapshot_in_flight || pending_snapshots.is_empty() {
        return Ok(());
    }
    session.handle_input(Input::Command(Command::Snapshot(SnapshotRequest {
        channel,
    })))?;
    *snapshot_in_flight = true;
    Ok(())
}

fn random_event_id() -> String {
    format!("{:032x}", rand::random::<u128>())
}

fn unix_time_ms() -> i64 {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(milliseconds).unwrap_or(i64::MAX)
}

fn drain_outputs_simple(
    session: &mut BcSession,
    wire: &mut dyn BaichuanTransport,
    buf: &mut Vec<u8>,
) -> anyhow::Result<Instant> {
    loop {
        match session.poll_output(buf) {
            Ok(Output::TcpSend { data }) => {
                wire.send(data)?;
            }
            Ok(Output::Event(_)) => {}
            Ok(Output::Timeout(deadline)) => return Ok(deadline),
            Err(reo_proto::BcError::BufferTooSmall { needed, .. }) => {
                buf.resize(needed, 0);
            }
            Err(e) => return Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bcudp_discovery_observes_pre_cancelled_shutdown() {
        let shutdown = Shutdown::new();
        shutdown.cancel();

        let result = UdpBaichuanTransport::connect(
            "192.0.2.1".parse().unwrap(),
            "camera-uid",
            None,
            &shutdown,
        );

        let Err(error) = result else {
            panic!("cancelled BCUDP discovery unexpectedly connected");
        };
        assert!(error.to_string().contains("cancelled"));
    }

    #[test]
    fn alarm_kinds_are_normalized_without_discarding_unknown_types() {
        assert_eq!(normalize_alarm_kind("MD"), "motion");
        assert_eq!(normalize_alarm_kind("people"), "person");
        assert_eq!(normalize_alarm_kind("dog_cat"), "animal");
        assert_eq!(normalize_alarm_kind("vehicle"), "vehicle");
    }

    #[test]
    fn alarm_event_kinds_include_supported_ai_types_only() {
        let alarm = AlarmEventData {
            status: "MD".try_into().unwrap(),
            ai_types: "people,vehicle".try_into().unwrap(),
            ..AlarmEventData::default()
        };

        assert_eq!(alarm_event_kinds(&alarm, false), vec!["person", "vehicle"]);
        assert_eq!(
            alarm_event_kinds(&alarm, true),
            vec!["motion", "person", "vehicle"]
        );
    }

    #[test]
    fn generic_motion_alarm_has_no_timeline_kind() {
        let alarm = AlarmEventData {
            status: "MD".try_into().unwrap(),
            ..AlarmEventData::default()
        };

        assert!(alarm_event_kinds(&alarm, false).is_empty());
        assert_eq!(alarm_event_kinds(&alarm, true), vec!["motion"]);
    }

    #[test]
    fn inactive_alarm_event_has_no_timeline_kinds() {
        let alarm = AlarmEventData {
            status: "none".try_into().unwrap(),
            ai_types: "none".try_into().unwrap(),
            ..AlarmEventData::default()
        };

        assert!(alarm_event_kinds(&alarm, false).is_empty());
    }

    #[test]
    fn inactive_legacy_alarm_does_not_reopen_its_alarm_type() {
        let alarm = AlarmEventData {
            alarm_type: "motion".try_into().unwrap(),
            status: "0".try_into().unwrap(),
            ..AlarmEventData::default()
        };

        assert!(alarm_event_kinds(&alarm, false).is_empty());
    }

    #[test]
    fn reconnect_ends_all_active_camera_events() {
        let (tx, rx) = mpsc::sync_channel(2);
        let mut active_motion = HashMap::from([
            ("motion".to_owned(), "event-motion".to_owned()),
            ("person".to_owned(), "event-person".to_owned()),
        ]);

        end_active_motion_events(&tx, &mut active_motion, 123);

        assert!(active_motion.is_empty());
        let mut ended_ids = [
            match rx.recv().unwrap() {
                KeepPeekEvent::TimelineEventEnded { id, end_time_ms } => {
                    assert_eq!(end_time_ms, 123);
                    id
                }
                _ => panic!("expected TimelineEventEnded"),
            },
            match rx.recv().unwrap() {
                KeepPeekEvent::TimelineEventEnded { id, end_time_ms } => {
                    assert_eq!(end_time_ms, 123);
                    id
                }
                _ => panic!("expected TimelineEventEnded"),
            },
        ];
        ended_ids.sort();
        assert_eq!(ended_ids, ["event-motion", "event-person"]);
    }

    #[test]
    fn snapshot_requests_are_serialized() {
        let now = Instant::now();
        let mut session = BcSession::default_client(now);
        session.set_state(reo_proto::SessionState::Connected);
        let pending_snapshots =
            VecDeque::from([vec!["event-one".to_owned()], vec!["event-two".to_owned()]]);
        let mut snapshot_in_flight = false;

        request_next_snapshot(&mut session, 0, &pending_snapshots, &mut snapshot_in_flight)
            .unwrap();
        request_next_snapshot(&mut session, 0, &pending_snapshots, &mut snapshot_in_flight)
            .unwrap();

        let mut output = [0u8; 4096];
        let mut request_count = 0;
        while let Output::TcpSend { data } = session.poll_output(&mut output).unwrap() {
            let (header, _) = reo_proto::PacketHeader::parse(data).unwrap();
            if header.msg_id == reo_proto::COMMAND_SNAP {
                request_count += 1;
            }
        }
        assert_eq!(request_count, 1);
    }

    #[test]
    fn generated_event_ids_are_safe_filename_components() {
        let id = random_event_id();
        assert_eq!(id.len(), 32);
        assert!(id.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn reconnect_rebinds_protocol_stream_id_without_losing_ingress_history() {
        let meta = VideoMeta {
            encoding: VideoEncoding::H265,
            width: 3840,
            height: 2160,
            framerate: 25.0,
        };
        let mut streams = Vec::new();

        let now = Instant::now();
        let entry = bind_stream_entry(&mut streams, 1, StreamKind::Main, meta.clone(), now);
        entry.stats.on_video_frame(true, 512);
        assert_eq!(entry.timestamps.normalize(Duration::ZERO), Duration::ZERO);
        assert_eq!(
            entry.timestamps.normalize(Duration::from_secs(1)),
            Duration::from_secs(1)
        );

        reset_stream_bindings(&mut streams);
        assert_eq!(streams[0].stream_id, None);

        bind_stream_entry(&mut streams, 9, StreamKind::Main, meta, now);

        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0].stream_id, Some(9));
        let snapshot = streams[0].stats.snapshot();
        assert_eq!(snapshot.reconnects, 2);
        assert_eq!(snapshot.video_frames, 1);
        assert_eq!(
            streams[0].timestamps.normalize(Duration::ZERO),
            Duration::from_secs(2)
        );
    }

    #[test]
    fn metadata_is_applied_to_only_its_protocol_stream() {
        let now = Instant::now();
        let empty_meta = VideoMeta {
            encoding: VideoEncoding::H265,
            width: 0,
            height: 0,
            framerate: 0.0,
        };
        let mut streams = Vec::new();
        bind_stream_entry(&mut streams, 11, StreamKind::Main, empty_meta.clone(), now);
        bind_stream_entry(&mut streams, 22, StreamKind::Sub, empty_meta, now);

        let main = streams
            .iter_mut()
            .find(|entry| entry.stream_id == Some(11))
            .unwrap();
        update_stream_info(main, 3840, 2160, 25.0);
        main.stats.on_video_frame(true, 512);
        main.stats.on_video_frame(false, 256);

        let sub = streams
            .iter_mut()
            .find(|entry| entry.stream_id == Some(22))
            .unwrap();
        update_stream_info(sub, 640, 360, 15.0);

        assert_eq!(streams[0].meta.width, 3840);
        assert_eq!(streams[0].meta.height, 2160);
        assert_eq!(streams[0].meta.framerate, 25.0);
        assert_eq!(streams[1].meta.width, 640);
        assert_eq!(streams[1].meta.height, 360);
        assert_eq!(streams[1].meta.framerate, 15.0);
        assert_eq!(streams[0].stats.snapshot().jitter_samples, 1);
        assert_eq!(streams[1].stats.snapshot().jitter_samples, 0);
    }

    #[test]
    fn measured_fps_is_matched_to_a_supported_rate() {
        let supported = [25.0, 20.0, 15.0, 10.0, 7.0, 4.0, 2.0];

        assert_eq!(infer_supported_fps(24.6, &supported), Some(25.0));
        assert_eq!(infer_supported_fps(14.7, &supported), Some(15.0));
        assert_eq!(infer_supported_fps(9.9, &supported), Some(10.0));
        assert_eq!(infer_supported_fps(1.2, &supported), None);
    }

    #[test]
    fn media_watchdog_requires_video_not_merely_a_live_session() {
        let now = Instant::now();
        let mut streams = Vec::new();
        bind_stream_entry(
            &mut streams,
            1,
            StreamKind::Main,
            VideoMeta {
                encoding: VideoEncoding::H265,
                width: 3840,
                height: 2160,
                framerate: 25.0,
            },
            now,
        );
        bind_stream_entry(
            &mut streams,
            1,
            StreamKind::Main,
            VideoMeta {
                encoding: VideoEncoding::H265,
                width: 3840,
                height: 2160,
                framerate: 25.0,
            },
            now + MEDIA_IDLE_TIMEOUT,
        );

        assert_eq!(
            expired_media_stream(&streams, now + MEDIA_IDLE_TIMEOUT),
            Some(StreamKind::Main)
        );

        note_video_progress(&mut streams[0], now + MEDIA_IDLE_TIMEOUT);
        assert_eq!(
            expired_media_stream(&streams, now + MEDIA_IDLE_TIMEOUT),
            None
        );
        assert_eq!(
            expired_media_stream(&streams, now + MEDIA_IDLE_TIMEOUT * 2),
            Some(StreamKind::Main)
        );
    }
}
