use crate::{
    cameras::{AudioEncoding, SessionTimestampNormalizer, VideoEncoding},
    keeppeek::{AudioMeta, KeepPeekEvent, StreamKind, VideoMeta},
    shutdown::Shutdown,
    stats::{
        CameraReport, HealthRegistry, IngressSnapshot, IngressStats, REPORT_INTERVAL, audio_report,
        log_camera_report, video_report,
    },
    storage::{
        AudioCodec, AudioFrame, MediaFrame, RecordingFrame, StorageHandle, VideoCodec, VideoFrame,
    },
    webrtc::{Publisher, Source},
};
use bytes::Bytes;
use retina::{
    client::{
        Credentials,
        core::{
            ClientOptions, ClientState, Command, Event, Input, Output, RtspClient, TcpConnectionId,
            Time, UdpPacketKind,
        },
    },
    codec::{CodecItem, ParametersRef, VideoParametersCodec},
};
use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket},
    sync::mpsc::SyncSender,
    time::{Duration, Instant, SystemTime},
};
use url::Url;

const RECONNECT_DELAY: Duration = Duration::from_secs(5);
const TCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const MEDIA_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const SOCKET_POLL_INTERVAL: Duration = Duration::from_millis(250);
const TCP_READ_BUFFER_SIZE: usize = 1024 * 1024;
const UDP_READ_BUFFER_SIZE: usize = 65_536;
const UDP_RECEIVE_BUFFER_SIZE: usize = 4 * 1024 * 1024;
const UDP_TCP_POLL_INTERVAL: Duration = Duration::from_millis(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtspTransport {
    Tcp,
    Udp,
}

struct UdpSockets {
    rtp: UdpSocket,
    rtcp: UdpSocket,
}

struct VideoContinuity {
    awaiting_random_access: bool,
}

impl VideoContinuity {
    const fn new() -> Self {
        Self {
            awaiting_random_access: false,
        }
    }

    const fn accepts(&mut self, loss: u16, is_random_access_point: bool) -> bool {
        if loss > 0 {
            self.awaiting_random_access = true;
            return false;
        }
        if self.awaiting_random_access {
            if !is_random_access_point {
                return false;
            }
            self.awaiting_random_access = false;
        }
        true
    }
}

struct BlockingRtsp {
    client: RtspClient,
    shutdown: Shutdown,
    connection: Option<(TcpConnectionId, TcpStream)>,
    deadline: Option<Instant>,
    items: VecDeque<CodecItem>,
    read_buffer: Vec<u8>,
    udp_streams: HashMap<usize, UdpSockets>,
}

impl BlockingRtsp {
    #[cfg(test)]
    fn describe(url: Url, credentials: Credentials) -> anyhow::Result<Self> {
        Self::describe_with_shutdown(url, credentials, Shutdown::new())
    }

    fn describe_with_shutdown(
        url: Url,
        credentials: Credentials,
        shutdown: Shutdown,
    ) -> anyhow::Result<Self> {
        let mut driver = Self {
            client: RtspClient::new(ClientOptions {
                credentials: Some(credentials),
                ..ClientOptions::default()
            }),
            shutdown,
            connection: None,
            deadline: None,
            items: VecDeque::new(),
            read_buffer: vec![0; TCP_READ_BUFFER_SIZE],
            udp_streams: HashMap::new(),
        };
        driver.apply(Input::Command {
            time: now(),
            command: Command::Describe { url },
        })?;
        driver.wait_for_state(ClientState::Described)?;
        Ok(driver)
    }

    fn streams(&self) -> &[retina::client::Stream] {
        self.client
            .streams()
            .expect("described RTSP client retains its presentation")
    }

    fn setup(&mut self, stream: usize, transport: RtspTransport) -> anyhow::Result<()> {
        let command = match transport {
            RtspTransport::Tcp => Command::Setup { stream },
            RtspTransport::Udp => {
                let sockets = self.bind_udp_pair()?;
                let client_port = sockets.rtp.local_addr()?.port();
                self.udp_streams.insert(stream, sockets);
                Command::SetupUdp {
                    stream,
                    client_port,
                }
            }
        };
        self.apply(Input::Command {
            time: now(),
            command,
        })?;
        self.wait_for_state(ClientState::Described)
    }

    fn bind_udp_pair(&self) -> anyhow::Result<UdpSockets> {
        let local_ip = self
            .connection
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("RTSP core has no active TCP connection"))?
            .1
            .local_addr()?
            .ip();
        for _ in 0..100 {
            let rtp = UdpSocket::bind(SocketAddr::new(local_ip, 0))?;
            let rtp_port = rtp.local_addr()?.port();
            let Some(rtcp_port) = rtp_port.checked_add(1) else {
                continue;
            };
            let Ok(rtcp) = UdpSocket::bind(SocketAddr::new(local_ip, rtcp_port)) else {
                continue;
            };
            rtp.set_nonblocking(true)?;
            rtcp.set_nonblocking(true)?;
            socket2::SockRef::from(&rtp).set_recv_buffer_size(UDP_RECEIVE_BUFFER_SIZE)?;
            socket2::SockRef::from(&rtcp).set_recv_buffer_size(UDP_RECEIVE_BUFFER_SIZE)?;
            return Ok(UdpSockets { rtp, rtcp });
        }
        anyhow::bail!("unable to bind adjacent UDP ports for RTP and RTCP")
    }

    fn play(&mut self) -> anyhow::Result<()> {
        self.apply(Input::Command {
            time: now(),
            command: Command::Play,
        })?;
        self.wait_for_state(ClientState::Playing)
    }

    fn next_item(
        &mut self,
        shutdown: &Shutdown,
        deadline: Option<Instant>,
    ) -> anyhow::Result<Option<CodecItem>> {
        loop {
            if let Some(item) = self.items.pop_front() {
                return Ok(Some(item));
            }
            if shutdown.is_cancelled() {
                return Ok(None);
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                anyhow::bail!("RTSP video stream made no media progress before its deadline")
            }
            self.pump_socket()?;
        }
    }

    fn apply(&mut self, input: Input<'_>) -> anyhow::Result<()> {
        self.client.handle_input(input)?;
        self.drain_outputs()
    }

    fn wait_for_state(&mut self, expected: ClientState) -> anyhow::Result<()> {
        loop {
            if self.shutdown.is_cancelled() {
                anyhow::bail!("RTSP operation cancelled")
            }
            match self.client.state() {
                state if state == expected => return Ok(()),
                state @ (ClientState::Failed | ClientState::Closed) => {
                    anyhow::bail!("RTSP client ended in {state:?} while waiting for {expected:?}")
                }
                _ => self.pump_socket()?,
            }
        }
    }

    fn drain_outputs(&mut self) -> anyhow::Result<()> {
        loop {
            match self.client.poll_output() {
                Output::OpenTcp { connection, target } => {
                    let stream = self.connect_tcp(target.host.as_ref(), target.port)?;
                    stream.set_nodelay(true)?;
                    self.connection = Some((connection, stream));
                    self.client.handle_input(Input::TcpConnected {
                        time: now(),
                        connection,
                    })?;
                }
                Output::TcpTransmit { connection, data } => {
                    {
                        let stream = self.connection_for(connection)?;
                        stream.write_all(&data)?;
                        stream.flush()?;
                    }
                    self.client.handle_input(Input::TcpWriteCompleted {
                        time: now(),
                        connection,
                    })?;
                }
                Output::CloseTcp { connection } => {
                    if self
                        .connection
                        .as_ref()
                        .is_some_and(|(actual, _)| *actual == connection)
                    {
                        let _ = self.connection.take();
                    }
                }
                Output::Event(event) => self.handle_event(event)?,
                Output::Timeout(deadline) => {
                    self.deadline = deadline;
                    return Ok(());
                }
                _ => anyhow::bail!("RTSP core emitted unsupported caller work"),
            }
        }
    }

    fn connect_tcp(&self, host: &str, port: u16) -> anyhow::Result<TcpStream> {
        let deadline = Instant::now() + TCP_CONNECT_TIMEOUT;
        let mut last_error = None;
        for address in (host, port).to_socket_addrs()? {
            if self.shutdown.is_cancelled() {
                anyhow::bail!("RTSP operation cancelled")
            }
            let Some(timeout) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            match TcpStream::connect_timeout(&address, timeout) {
                Ok(stream) => {
                    if self.shutdown.is_cancelled() {
                        anyhow::bail!("RTSP operation cancelled")
                    }
                    return Ok(stream);
                }
                Err(error) => last_error = Some(error),
            }
        }
        match last_error {
            Some(error) => Err(error.into()),
            None => anyhow::bail!("RTSP target {host}:{port} resolved to no addresses"),
        }
    }

    fn handle_event(&mut self, event: Event) -> anyhow::Result<()> {
        match event {
            Event::CodecItem(item) => self.items.push_back(item),
            Event::DescribeResponse { response, .. }
                if !response.status_code.is_success()
                    && self.client.state() == ClientState::Failed =>
            {
                anyhow::bail!(
                    "RTSP DESCRIBE failed with {} {}",
                    response.status_code,
                    response.reason_phrase
                );
            }
            Event::SetupResponse { response, .. }
                if !response.status_code.is_success()
                    && self.client.state() == ClientState::Failed =>
            {
                anyhow::bail!(
                    "RTSP SETUP failed with {} {}",
                    response.status_code,
                    response.reason_phrase
                );
            }
            Event::UdpSetup {
                stream,
                source,
                server_port,
            } => {
                let source = source.unwrap_or(
                    self.connection
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("RTSP TCP connection is unavailable"))?
                        .1
                        .peer_addr()?
                        .ip(),
                );
                let sockets = self.udp_streams.get(&stream).ok_or_else(|| {
                    anyhow::anyhow!("RTSP core configured unknown UDP stream {stream}")
                })?;
                sockets.rtp.connect(SocketAddr::new(source, server_port))?;
                sockets.rtcp.connect(SocketAddr::new(
                    source,
                    server_port
                        .checked_add(1)
                        .ok_or_else(|| anyhow::anyhow!("invalid RTCP server port"))?,
                ))?;
            }
            Event::PlayResponse { response, .. }
                if !response.status_code.is_success()
                    && self.client.state() == ClientState::Failed =>
            {
                anyhow::bail!(
                    "RTSP PLAY failed with {} {}",
                    response.status_code,
                    response.reason_phrase
                );
            }
            Event::TcpConnectFailed { error, .. } => {
                anyhow::bail!("RTSP TCP connection failed: {error}")
            }
            Event::TcpClosed { .. } => anyhow::bail!("RTSP TCP connection closed"),
            Event::RequestTimedOut { cseq, .. } => {
                anyhow::bail!("RTSP request CSeq {cseq} timed out")
            }
            _ => {}
        }
        Ok(())
    }

    fn pump_socket(&mut self) -> anyhow::Result<()> {
        if self.pump_udp()? {
            return Ok(());
        }
        let timeout = self
            .deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .map(|timeout| timeout.min(SOCKET_POLL_INTERVAL))
            .unwrap_or(SOCKET_POLL_INTERVAL)
            .min(if self.udp_streams.is_empty() {
                SOCKET_POLL_INTERVAL
            } else {
                UDP_TCP_POLL_INTERVAL
            })
            .max(Duration::from_millis(1));
        let mut buffer = std::mem::take(&mut self.read_buffer);
        let read = {
            let (_, stream) = self
                .connection
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("RTSP core has no active TCP connection"))?;
            stream.set_read_timeout(Some(timeout))?;
            stream.read(&mut buffer)
        };
        match read {
            Ok(0) => {
                let connection = self.active_connection()?;
                self.apply(Input::TcpClosed {
                    time: now(),
                    connection,
                })?;
            }
            Ok(len) => {
                let connection = self.active_connection()?;
                self.apply(Input::TcpData {
                    time: now(),
                    connection,
                    data: &buffer[..len],
                })?;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                self.client.handle_input(Input::Timeout { time: now() })?;
                self.drain_outputs()?;
            }
            Err(error) => return Err(error.into()),
        };
        self.read_buffer = buffer;
        self.pump_udp()?;
        Ok(())
    }

    fn pump_udp(&mut self) -> anyhow::Result<bool> {
        let mut datagrams = Vec::new();
        let mut buffer = [0u8; UDP_READ_BUFFER_SIZE];
        for (&stream, sockets) in &self.udp_streams {
            for (kind, socket) in [
                (UdpPacketKind::Rtcp, &sockets.rtcp),
                (UdpPacketKind::Rtp, &sockets.rtp),
            ] {
                loop {
                    match socket.recv(&mut buffer) {
                        Ok(len) => datagrams.push((stream, kind, buffer[..len].to_vec())),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                            break;
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
            }
        }
        let received = !datagrams.is_empty();
        for (stream, kind, data) in datagrams {
            self.apply(Input::UdpData {
                time: now(),
                stream,
                kind,
                data: &data,
            })?;
        }
        Ok(received)
    }

    fn active_connection(&self) -> anyhow::Result<TcpConnectionId> {
        self.connection
            .as_ref()
            .map(|(connection, _)| *connection)
            .ok_or_else(|| anyhow::anyhow!("RTSP core has no active TCP connection"))
    }

    fn connection_for(&mut self, connection: TcpConnectionId) -> anyhow::Result<&mut TcpStream> {
        self.connection
            .as_mut()
            .filter(|(actual, _)| *actual == connection)
            .map(|(_, stream)| stream)
            .ok_or_else(|| anyhow::anyhow!("RTSP core requested an unknown TCP connection"))
    }
}

fn now() -> Time {
    Time {
        monotonic: Instant::now(),
        wall: SystemTime::now(),
    }
}

pub struct RtspLoop {
    pub camera_ip: IpAddr,
    pub camera_name: Option<String>,
    pub camera_brand: Option<String>,
    pub camera_port: u16,
    pub stream: StreamKind,
    pub rtsp_url: String,
    pub username: String,
    pub password: String,
    pub transport: RtspTransport,
    pub video_meta: VideoMeta,
    pub audio_meta: Option<AudioMeta>,
    pub storage: Option<StorageHandle>,
    pub live: Option<Publisher>,
    pub health: HealthRegistry,
    pub tx: SyncSender<KeepPeekEvent>,
    pub shutdown: Shutdown,
}

impl RtspLoop {
    fn storage_label(&self) -> String {
        self.camera_name
            .as_deref()
            .unwrap_or(&self.camera_ip.to_string())
            .to_owned()
    }

    pub fn run(self) {
        let mut stats = IngressStats::new();
        let mut previous = stats.snapshot();
        let mut last_report = Instant::now();
        let mut video_timestamps = SessionTimestampNormalizer::new();
        let mut audio_timestamps = SessionTimestampNormalizer::new();

        while !self.shutdown.is_cancelled() {
            match self.run_stream(
                &mut stats,
                &mut previous,
                &mut last_report,
                &mut video_timestamps,
                &mut audio_timestamps,
            ) {
                Ok(()) => break,
                Err(error) => {
                    stats.on_error();
                    let _ = self.tx.send(KeepPeekEvent::StreamError {
                        camera_ip: self.camera_ip,
                        stream: self.stream,
                        error: error.to_string(),
                    });
                    tracing::info!(
                        ip = %self.camera_ip,
                        stream = %self.stream,
                        error = %error,
                        "reconnecting RTSP stream",
                    );
                    if self.shutdown.wait_timeout(RECONNECT_DELAY) {
                        break;
                    }
                }
            }
        }
    }

    fn run_stream(
        &self,
        stats: &mut IngressStats,
        previous: &mut IngressSnapshot,
        last_report: &mut Instant,
        video_timestamps: &mut SessionTimestampNormalizer,
        audio_timestamps: &mut SessionTimestampNormalizer,
    ) -> anyhow::Result<()> {
        let mut url = Url::parse(&self.rtsp_url)?;
        url.set_username("").ok();
        url.set_password(None).ok();

        let (mut driver, active_url) = match self.try_describe(&url) {
            Ok(driver) => (driver, url.clone()),
            Err(error) if error.to_string().contains("Not Found") => {
                let url_string = url.as_str();
                if !url_string.contains("h265Preview") {
                    return Err(error);
                }
                let fallback = Url::parse(&url_string.replace("h265Preview", "h264Preview"))?;
                tracing::info!(
                    ip = %self.camera_ip,
                    stream = %self.stream,
                    url = %fallback,
                    "h265 not found, falling back to h264",
                );
                (self.try_describe(&fallback)?, fallback)
            }
            Err(error) => return Err(error),
        };

        let mut video_meta = self.video_meta.clone();
        if let Some(ParametersRef::Video(parameters)) = driver.streams()[0].parameters() {
            video_meta.encoding = match parameters.codec_params() {
                VideoParametersCodec::H264 { .. } => VideoEncoding::H264,
                VideoParametersCodec::H265 { .. } => VideoEncoding::H265,
                VideoParametersCodec::Jpeg => VideoEncoding::JPEG,
                _ => video_meta.encoding,
            };
            (video_meta.width, video_meta.height) = parameters.pixel_dimensions();
            if let Some((numerator, denominator)) = parameters.frame_rate()
                && numerator > 0
            {
                video_meta.framerate = f64::from(denominator) / f64::from(numerator);
            }
        }

        driver.setup(0, self.transport)?;
        for (index, stream) in driver.streams().iter().enumerate() {
            tracing::debug!(
                ip = %self.camera_ip,
                stream = %self.stream,
                track = index,
                params = ?stream.parameters(),
                "rtsp track",
            );
        }
        for index in 1..driver.streams().len() {
            if let Err(error) = driver.setup(index, self.transport) {
                tracing::debug!(
                    ip = %self.camera_ip,
                    stream = %self.stream,
                    track = index,
                    %error,
                    "skipping RTSP track",
                );
            }
        }
        driver.play()?;
        video_timestamps.begin_session();
        audio_timestamps.begin_session();

        let _ = self.tx.send(KeepPeekEvent::StreamConnected {
            camera_ip: self.camera_ip,
            stream: self.stream,
        });
        tracing::info!(
            ip = %self.camera_ip,
            stream = %self.stream,
            url = %active_url,
            "rtsp connected",
        );

        stats.on_connect();
        stats.set_stream_info(
            video_meta.encoding.clone(),
            video_meta.width,
            video_meta.height,
            video_meta.framerate,
        );
        let mut video_continuity = VideoContinuity::new();
        let mut video_deadline = Instant::now() + MEDIA_IDLE_TIMEOUT;

        while let Some(item) = driver.next_item(&self.shutdown, Some(video_deadline))? {
            match item {
                CodecItem::VideoFrame(frame) => {
                    video_deadline = Instant::now() + MEDIA_IDLE_TIMEOUT;
                    let loss = frame.loss();
                    if loss > 0 {
                        stats.dropped_frames = stats.dropped_frames.wrapping_add(u64::from(loss));
                    }
                    let is_keyframe = frame.is_random_access_point();
                    if !video_continuity.accepts(loss, is_keyframe) {
                        continue;
                    }
                    let timestamp = frame.timestamp().elapsed_duration();
                    let data = Bytes::from(frame.into_data());
                    stats.on_video_frame(is_keyframe, data.len());
                    self.report_if_due(stats, previous, last_report);
                    let frame_codec = match video_meta.encoding {
                        VideoEncoding::H264 => Some(VideoCodec::H264),
                        VideoEncoding::H265 => Some(VideoCodec::H265),
                        _ => None,
                    };
                    if let Some(codec) = frame_codec {
                        let received_at = Instant::now();
                        let timestamp =
                            timestamp.map(|timestamp| video_timestamps.normalize(timestamp));
                        if let Some(live) = &self.live {
                            live.publish(
                                Source {
                                    camera_ip: self.camera_ip,
                                    stream: self.stream,
                                },
                                codec,
                                is_keyframe,
                                received_at,
                                timestamp,
                                data.clone(),
                            );
                        }
                        let frame = MediaFrame::Video(VideoFrame {
                            codec,
                            is_keyframe,
                            width: video_meta.width,
                            height: video_meta.height,
                            data,
                        });
                        if let Some(storage) = &self.storage {
                            let camera_id = format!("{}/{}", self.storage_label(), self.stream);
                            storage.ingest(
                                &camera_id,
                                RecordingFrame {
                                    received_at,
                                    timestamp,
                                    frame,
                                },
                            );
                        }
                    }
                }
                CodecItem::AudioFrame(frame) => {
                    if let Some(audio_meta) = &self.audio_meta {
                        let timestamp = frame.timestamp().elapsed_duration();
                        let duration = frame.duration();
                        let data = frame.data().to_vec();
                        stats.on_audio_frame(data.len());
                        let frame_codec = match audio_meta.encoding {
                            AudioEncoding::AAC => Some(AudioCodec::Aac),
                            AudioEncoding::G711 => Some(AudioCodec::G711Alaw),
                            _ => None,
                        };
                        if let Some(codec) = frame_codec {
                            let sample_rate = audio_meta
                                .sample_rate
                                .unwrap_or_else(|| codec.default_sample_rate());
                            let frame = MediaFrame::Audio(AudioFrame {
                                codec,
                                sample_rate,
                                duration,
                                data,
                            });
                            if let Some(storage) = &self.storage {
                                let camera_id = format!("{}/{}", self.storage_label(), self.stream);
                                let timestamp = timestamp
                                    .map(|timestamp| audio_timestamps.normalize(timestamp));
                                storage.ingest(
                                    &camera_id,
                                    RecordingFrame {
                                        received_at: Instant::now(),
                                        timestamp,
                                        frame,
                                    },
                                );
                            }
                        }
                    }
                }
                CodecItem::MessageFrame(_) | CodecItem::Rtcp(_) => {}
                _ => tracing::debug!("ignoring unsupported RTSP codec item"),
            }
        }
        Ok(())
    }

    fn try_describe(&self, url: &Url) -> anyhow::Result<BlockingRtsp> {
        BlockingRtsp::describe_with_shutdown(
            url.clone(),
            Credentials {
                username: self.username.clone(),
                password: self.password.clone(),
            },
            self.shutdown.clone(),
        )
    }

    fn report_if_due(
        &self,
        stats: &mut IngressStats,
        previous: &mut IngressSnapshot,
        last_report: &mut Instant,
    ) {
        if last_report.elapsed() < REPORT_INTERVAL {
            return;
        }
        *last_report = Instant::now();
        let snapshot = stats.snapshot();
        let rates = snapshot.rates_since(previous);
        let mut streams = vec![video_report(
            self.stream,
            &snapshot,
            &rates,
            stats.codec.as_ref(),
            stats.width,
            stats.height,
            stats.expected_fps,
        )];
        if let Some(audio) = audio_report(
            &snapshot,
            &rates,
            self.audio_meta
                .as_ref()
                .map(|audio| audio.encoding.to_string())
                .as_deref(),
        ) {
            streams.push(audio);
        }
        let report = CameraReport {
            ip: self.camera_ip,
            name: self.camera_name.clone(),
            brand: self.camera_brand.clone(),
            port: self.camera_port,
            streams,
        };
        log_camera_report(&report);
        self.health.publish(report);
        *previous = snapshot;
    }
}

#[cfg(test)]
mod tests {
    use super::{BlockingRtsp, RtspTransport, VideoContinuity};
    use crate::shutdown::Shutdown;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use bytes::Bytes;
    use mp4::{
        AvcConfig, HevcConfig, MediaConfig, Mp4Config, Mp4Sample, Mp4Writer, TrackConfig, TrackType,
    };
    use retina::{client::Credentials, codec::CodecItem, server::RtspServer};
    use std::{
        fs::{self, File},
        net::TcpListener,
        path::{Path, PathBuf},
        sync::{
            atomic::{AtomicU64, Ordering},
            mpsc,
        },
        time::{Duration, Instant},
    };
    use url::Url;

    static NEXT_TEST_FILE: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn video_continuity_waits_for_random_access_after_loss() {
        let mut continuity = VideoContinuity::new();

        assert!(continuity.accepts(0, false));
        assert!(!continuity.accepts(1, false));
        assert!(!continuity.accepts(0, false));
        assert!(continuity.accepts(0, true));
        assert!(continuity.accepts(0, false));
    }

    #[test]
    fn video_continuity_rejects_lossy_random_access_frame() {
        let mut continuity = VideoContinuity::new();

        assert!(!continuity.accepts(1, true));
        assert!(!continuity.accepts(0, false));
        assert!(continuity.accepts(0, true));
    }

    #[test]
    fn blocking_client_describe_observes_shutdown_while_camera_is_silent() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (_socket, _) = listener.accept().unwrap();
            accepted_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        let shutdown = Shutdown::new();
        let client_shutdown = shutdown.clone();
        let client = std::thread::spawn(move || {
            BlockingRtsp::describe_with_shutdown(
                Url::parse(&format!("rtsp://{address}/silent")).unwrap(),
                Credentials {
                    username: "operator".to_owned(),
                    password: "swordfish".to_owned(),
                },
                client_shutdown,
            )
        });
        accepted_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let started = Instant::now();
        shutdown.cancel();
        let error = match client.join().unwrap() {
            Ok(_) => panic!("RTSP describe completed while the camera was silent"),
            Err(error) => error,
        };
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(error.to_string().contains("cancelled"));

        release_tx.send(()).unwrap();
        server.join().unwrap();
    }

    struct TemporaryMp4(PathBuf);

    impl TemporaryMp4 {
        fn h264(width: u16, height: u16) -> Self {
            let path = std::env::temp_dir().join(format!(
                "keeppeek-fake-rtsp-{}-{}.mp4",
                std::process::id(),
                NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed),
            ));
            let mut writer = Mp4Writer::write_start(
                File::create(&path).unwrap(),
                &Mp4Config {
                    major_brand: "isom".parse().unwrap(),
                    minor_version: 512,
                    compatible_brands: vec![
                        "isom".parse().unwrap(),
                        "iso2".parse().unwrap(),
                        "avc1".parse().unwrap(),
                        "mp41".parse().unwrap(),
                    ],
                    timescale: 90_000,
                },
            )
            .unwrap();
            writer
                .add_track(&TrackConfig {
                    track_type: TrackType::Video,
                    timescale: 90_000,
                    language: "und".to_string(),
                    media_conf: MediaConfig::AvcConfig(AvcConfig {
                        width,
                        height,
                        seq_param_set: STANDARD
                            .decode("Z00AKZpkA8ARPyzUBAQFAAADA+gAAOpgBA==")
                            .unwrap(),
                        pic_param_set: STANDARD.decode("aO48gA==").unwrap(),
                    }),
                })
                .unwrap();
            let mut fragmented_sample = Vec::with_capacity(2_005);
            fragmented_sample.extend_from_slice(&2_001_u32.to_be_bytes());
            fragmented_sample.push(0x41);
            fragmented_sample.extend(vec![0x55; 2_000]);
            for (start_time, is_sync, bytes) in [
                (
                    0,
                    true,
                    Bytes::from_static(&[0x00, 0x00, 0x00, 0x04, 0x65, 0x88, 0x84, 0x21]),
                ),
                (3_000, false, Bytes::from(fragmented_sample)),
            ] {
                writer
                    .write_sample(
                        1,
                        &Mp4Sample {
                            start_time,
                            duration: 3_000,
                            rendering_offset: 0,
                            is_sync,
                            bytes,
                        },
                    )
                    .unwrap();
            }
            writer.write_end().unwrap();
            Self(path)
        }

        fn h265(width: u16, height: u16) -> Self {
            let path = std::env::temp_dir().join(format!(
                "keeppeek-fake-rtsp-{}-{}.mp4",
                std::process::id(),
                NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed),
            ));
            let mut writer = Mp4Writer::write_start(
                File::create(&path).unwrap(),
                &Mp4Config {
                    major_brand: "isom".parse().unwrap(),
                    minor_version: 512,
                    compatible_brands: vec![
                        "isom".parse().unwrap(),
                        "iso6".parse().unwrap(),
                        "hev1".parse().unwrap(),
                        "mp41".parse().unwrap(),
                    ],
                    timescale: 90_000,
                },
            )
            .unwrap();
            writer
                .add_track(&TrackConfig {
                    track_type: TrackType::Video,
                    timescale: 90_000,
                    language: "und".to_string(),
                    media_conf: MediaConfig::HevcConfig(HevcConfig {
                        width,
                        height,
                        vps: STANDARD
                            .decode("QAEMAf//AWAAAAMAsAAAAwAAAwBarAwAAAMABAAAAwAyqA==")
                            .unwrap(),
                        sps: STANDARD
                            .decode("QgEBAWAAAAMAsAAAAwAAAwBaoAWCAeFja5JFL83BQYFBAAADAAEAAAMADKE=")
                            .unwrap(),
                        pps: STANDARD.decode("RAHA8saNA7NA").unwrap(),
                    }),
                })
                .unwrap();
            let mut fragmented_sample = Vec::with_capacity(2_006);
            fragmented_sample.extend_from_slice(&2_002_u32.to_be_bytes());
            fragmented_sample.extend_from_slice(&[0x02, 0x01]);
            fragmented_sample.extend(vec![0x55; 2_000]);
            for (start_time, is_sync, bytes) in [
                (
                    0,
                    true,
                    Bytes::from_static(&[0x00, 0x00, 0x00, 0x05, 0x26, 0x01, b'i', b'd', b'r']),
                ),
                (3_000, false, Bytes::from(fragmented_sample)),
            ] {
                writer
                    .write_sample(
                        1,
                        &Mp4Sample {
                            start_time,
                            duration: 3_000,
                            rendering_offset: 0,
                            is_sync,
                            bytes,
                        },
                    )
                    .unwrap();
            }
            writer.write_end().unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TemporaryMp4 {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    #[test]
    fn blocking_client_completes_a_fake_camera_playback() {
        let camera = RtspServer::start().unwrap();
        let mut client = BlockingRtsp::describe(
            camera.url(),
            Credentials {
                username: "operator".to_string(),
                password: "swordfish".to_string(),
            },
        )
        .unwrap();

        assert_eq!(client.streams().len(), 1);
        client.setup(0, RtspTransport::Tcp).unwrap();
        client.play().unwrap();

        let shutdown = Shutdown::new();
        let item = client.next_item(&shutdown, None).unwrap().unwrap();
        match item {
            CodecItem::VideoFrame(frame) => {
                assert!(frame.is_random_access_point());
                assert!(!frame.data().is_empty());
            }
            item => panic!("expected fake camera video frame, got {item:?}"),
        }
        drop(client);

        let transcript = camera.finish().unwrap();
        let methods = transcript
            .requests()
            .iter()
            .map(|request| request.method.to_string())
            .collect::<Vec<_>>();
        assert_eq!(methods, ["DESCRIBE", "SETUP", "PLAY"]);
        let cseqs = transcript
            .requests()
            .iter()
            .map(|request| request.headers.get("CSeq").unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(cseqs, ["1", "2", "3"]);
    }

    #[test]
    fn blocking_client_times_out_when_camera_stalls_with_open_connection() {
        let camera = RtspServer::start_stalled_after_first_packet().unwrap();
        let mut client = BlockingRtsp::describe(
            camera.url(),
            Credentials {
                username: "operator".to_string(),
                password: "swordfish".to_string(),
            },
        )
        .unwrap();
        client.setup(0, RtspTransport::Tcp).unwrap();
        client.play().unwrap();

        let shutdown = Shutdown::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        assert!(matches!(
            client.next_item(&shutdown, Some(deadline)).unwrap(),
            Some(CodecItem::VideoFrame(_))
        ));
        let error = client
            .next_item(&shutdown, Some(Instant::now() + Duration::from_millis(100)))
            .unwrap_err();
        assert!(error.to_string().contains("no media progress"));

        drop(client);
        camera.finish().unwrap();
    }

    #[test]
    fn blocking_client_streams_h264_mp4_source() {
        let source = TemporaryMp4::h264(1_920, 1_080);
        let camera = RtspServer::from_mp4(source.path()).unwrap();
        let mut client = BlockingRtsp::describe(
            camera.url(),
            Credentials {
                username: "operator".to_string(),
                password: "swordfish".to_string(),
            },
        )
        .unwrap();
        client.setup(0, RtspTransport::Tcp).unwrap();
        client.play().unwrap();

        let shutdown = Shutdown::new();
        let first = client.next_item(&shutdown, None).unwrap().unwrap();
        let second = client.next_item(&shutdown, None).unwrap().unwrap();
        let CodecItem::VideoFrame(first) = first else {
            panic!("expected first MP4 video frame");
        };
        let CodecItem::VideoFrame(second) = second else {
            panic!("expected second MP4 video frame");
        };
        assert_eq!(first.timestamp().elapsed(), 0);
        assert_eq!(second.timestamp().elapsed(), 3_000);
        assert!(second.data().len() > 2_000);
        drop(client);

        let transcript = camera.finish().unwrap();
        assert_eq!(transcript.requests().len(), 3);
    }

    #[test]
    fn blocking_client_streams_h264_mp4_source_over_udp() {
        let source = TemporaryMp4::h264(1_920, 1_080);
        let camera = RtspServer::from_mp4(source.path()).unwrap();
        let mut client = BlockingRtsp::describe(
            camera.url(),
            Credentials {
                username: "operator".to_string(),
                password: "swordfish".to_string(),
            },
        )
        .unwrap();
        client.setup(0, RtspTransport::Udp).unwrap();
        client.play().unwrap();

        let shutdown = Shutdown::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        assert!(matches!(
            client.next_item(&shutdown, Some(deadline)).unwrap(),
            Some(CodecItem::VideoFrame(_))
        ));
        drop(client);

        let transcript = camera.finish().unwrap();
        assert_eq!(transcript.requests().len(), 3);
    }

    #[test]
    fn blocking_client_streams_h265_mp4_source() {
        let source = TemporaryMp4::h265(1_920, 1_080);
        let camera = RtspServer::from_mp4(source.path()).unwrap();
        let mut client = BlockingRtsp::describe(
            camera.url(),
            Credentials {
                username: "operator".to_string(),
                password: "swordfish".to_string(),
            },
        )
        .unwrap();
        client.setup(0, RtspTransport::Tcp).unwrap();
        client.play().unwrap();

        let shutdown = Shutdown::new();
        let first = client.next_item(&shutdown, None).unwrap().unwrap();
        let second = client.next_item(&shutdown, None).unwrap().unwrap();
        let CodecItem::VideoFrame(first) = first else {
            panic!("expected first H.265 MP4 video frame");
        };
        let CodecItem::VideoFrame(second) = second else {
            panic!("expected second H.265 MP4 video frame");
        };
        assert!(first.is_random_access_point());
        assert_eq!(first.timestamp().elapsed(), 0);
        assert_eq!(second.timestamp().elapsed(), 3_000);
        assert!(second.data().len() > 2_000);
        drop(client);

        let transcript = camera.finish().unwrap();
        assert_eq!(transcript.requests().len(), 3);
    }

    #[test]
    fn blocking_clients_stream_high_and_low_mp4_profiles() {
        let high_source = TemporaryMp4::h265(1_920, 1_080);
        let low_source = TemporaryMp4::h264(640, 360);
        let camera = RtspServer::from_mp4_streams(high_source.path(), low_source.path()).unwrap();
        let shutdown = Shutdown::new();

        let mut high = BlockingRtsp::describe(
            camera.high_resolution_url(),
            Credentials {
                username: "operator".to_string(),
                password: "swordfish".to_string(),
            },
        )
        .unwrap();
        high.setup(0, RtspTransport::Tcp).unwrap();
        high.play().unwrap();
        assert!(matches!(
            high.next_item(&shutdown, None).unwrap(),
            Some(CodecItem::VideoFrame(_))
        ));
        drop(high);

        let mut low = BlockingRtsp::describe(
            camera.low_resolution_url(),
            Credentials {
                username: "operator".to_string(),
                password: "swordfish".to_string(),
            },
        )
        .unwrap();
        low.setup(0, RtspTransport::Tcp).unwrap();
        low.play().unwrap();
        assert!(matches!(
            low.next_item(&shutdown, None).unwrap(),
            Some(CodecItem::VideoFrame(_))
        ));
        drop(low);

        let transcript = camera.finish().unwrap();
        let describe_paths = transcript
            .requests()
            .iter()
            .filter(|request| request.method.to_string() == "DESCRIBE")
            .map(|request| request.request_uri.as_ref().unwrap().path().to_string())
            .collect::<Vec<_>>();
        assert_eq!(describe_paths, ["/high", "/low"]);
    }
}
