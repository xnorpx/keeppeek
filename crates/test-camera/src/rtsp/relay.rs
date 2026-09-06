use super::Metadata;
use anyhow::{Context, bail, ensure};
use retina::{client::core::RtspFramer, rtsp::msg};
use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const POLL_INTERVAL: Duration = Duration::from_millis(5);
const WRITE_TIMEOUT: Duration = Duration::from_millis(100);
const SETUP_TIMEOUT: Duration = Duration::from_secs(5);
const MESSAGE_BYTES_MAX: usize = 128 * 1024;
const REQUESTS_MAX: usize = 8;
const METADATA_PAYLOAD_MAX: usize = 1199;
const METADATA_PACKET_BATCH: usize = 32;
const METADATA_INTERVAL: Duration = Duration::from_millis(100);
const METADATA_TIMESTAMP_STEP: u32 = 9000;
const METADATA_SSRC: u32 = 0x4d45_5441;
const SESSION_ID: &str = "test-camera-metadata";

pub(super) struct Relay {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Relay {
    pub(super) fn start(upstream: SocketAddr, metadata: Arc<Metadata>) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(SocketAddr::new(upstream.ip(), 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let worker = thread::Builder::new()
            .name("test-camera-metadata".to_owned())
            .spawn(move || {
                while !stopped.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if let Err(error) = Session::new(stream, upstream)
                                .and_then(|mut session| session.run(&metadata, &stopped))
                            {
                                tracing::debug!(%error, "metadata RTSP session ended");
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::park_timeout(POLL_INTERVAL);
                        }
                        Err(error) => {
                            tracing::error!(%error, "metadata RTSP listener failed");
                            break;
                        }
                    }
                }
            })?;
        Ok(Self {
            address,
            stop,
            worker: Some(worker),
        })
    }

    pub(super) const fn address(&self) -> SocketAddr {
        self.address
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            if worker.join().is_err() {
                tracing::error!("metadata RTSP worker panicked");
            }
        }
    }
}

struct Peer {
    stream: TcpStream,
    framer: RtspFramer,
    pending_bytes: usize,
    write_buffer: Vec<u8>,
}

impl Peer {
    fn new(stream: TcpStream) -> anyhow::Result<Self> {
        stream.set_nonblocking(false)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(POLL_INTERVAL))?;
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        Ok(Self {
            stream,
            framer: RtspFramer::default(),
            pending_bytes: 0,
            write_buffer: Vec::with_capacity(16 * 1024),
        })
    }

    fn receive(&mut self) -> anyhow::Result<Option<Vec<retina::client::core::FramedMessage>>> {
        let mut buffer = [0; 16 * 1024];
        let length = match self.stream.read(&mut buffer) {
            Ok(0) => return Ok(None),
            Ok(length) => length,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(Some(Vec::new()));
            }
            Err(error) => return Err(error.into()),
        };
        self.pending_bytes += length;
        ensure!(
            self.pending_bytes <= MESSAGE_BYTES_MAX,
            "RTSP message exceeds relay buffer limit"
        );
        let messages = self.framer.push(&buffer[..length])?;
        if !messages.is_empty() {
            self.pending_bytes = length;
        }
        Ok(Some(messages))
    }

    fn send(&mut self, message: &msg::Message, body: &[u8]) -> anyhow::Result<()> {
        self.write_buffer.clear();
        match message {
            msg::Message::Request(request) => request.write_head(&mut self.write_buffer)?,
            msg::Message::Response(response) => response.write_head(&mut self.write_buffer)?,
            msg::Message::Data(data) => data.write(&mut self.write_buffer)?,
        }
        self.write_buffer.extend_from_slice(body);
        self.stream.write_all(&self.write_buffer)?;
        Ok(())
    }
}

struct Session {
    client: Peer,
    upstream: Peer,
    requests: VecDeque<msg::Request>,
    upstream_session: Option<msg::HeaderValue>,
    video_channel: Option<u8>,
    metadata_channel: Option<u8>,
    progress: Option<Progress>,
    playing: bool,
}

impl Session {
    fn new(client: TcpStream, upstream: SocketAddr) -> anyhow::Result<Self> {
        Ok(Self {
            client: Peer::new(client)?,
            upstream: Peer::new(TcpStream::connect_timeout(&upstream, WRITE_TIMEOUT)?)?,
            requests: VecDeque::with_capacity(REQUESTS_MAX),
            upstream_session: None,
            video_channel: None,
            metadata_channel: None,
            progress: None,
            playing: false,
        })
    }

    fn run(&mut self, metadata: &Metadata, stop: &AtomicBool) -> anyhow::Result<()> {
        let setup_deadline = Instant::now() + SETUP_TIMEOUT;
        let mut upstream_closed = false;
        while !stop.load(Ordering::Relaxed) {
            ensure!(
                self.playing || Instant::now() < setup_deadline,
                "RTSP setup timed out"
            );
            let Some(messages) = self.client.receive()? else {
                return Ok(());
            };
            for message in messages {
                if stop.load(Ordering::Relaxed) {
                    return Ok(());
                }
                match message.message {
                    msg::Message::Request(request) => {
                        if !self.request(request, &message.body)? {
                            return Ok(());
                        }
                    }
                    msg::Message::Data(data) => {
                        if self
                            .metadata_channel
                            .is_none_or(|channel| data.channel_id != channel + 1)
                        {
                            self.upstream
                                .send(&msg::Message::Data(data), &message.body)?;
                        }
                    }
                    _ => bail!("client sent an RTSP response"),
                }
            }
            if !upstream_closed {
                match self.upstream.receive()? {
                    Some(messages) => {
                        for message in messages {
                            if stop.load(Ordering::Relaxed) {
                                return Ok(());
                            }
                            self.response(message.message, &message.body, metadata)?;
                        }
                    }
                    None => upstream_closed = true,
                }
            }
            if let Some(progress) = &mut self.progress {
                progress.send_ready(&mut self.client.stream, metadata, stop)?;
            }
            if upstream_closed
                && self
                    .progress
                    .as_ref()
                    .is_none_or(|progress| progress.done(metadata))
            {
                return Ok(());
            }
        }
        Ok(())
    }

    fn request(&mut self, mut request: msg::Request, body: &[u8]) -> anyhow::Result<bool> {
        if request.method == msg::Method::TEARDOWN {
            self.reply(&request, msg::Headers::default())?;
            return Ok(false);
        }
        if request.method == msg::Method::OPTIONS || request.method == msg::Method::GET_PARAMETER {
            let mut headers = msg::Headers::default();
            headers.insert(
                msg::HeaderName::PUBLIC,
                "OPTIONS, DESCRIBE, SETUP, PLAY, GET_PARAMETER, TEARDOWN".try_into()?,
            );
            self.reply(&request, headers)?;
            return Ok(true);
        }
        if request.method == msg::Method::SETUP {
            let channel = interleaved_channel(&request)?;
            if request
                .request_uri
                .as_ref()
                .is_some_and(|url| url.path().ends_with("/trackID=1"))
            {
                ensure!(
                    self.video_channel
                        .is_none_or(|video| video.abs_diff(channel) > 1),
                    "metadata channels overlap video channels"
                );
                let mut headers = msg::Headers::default();
                headers.insert(
                    msg::HeaderName::TRANSPORT,
                    format!(
                        "RTP/AVP/TCP;unicast;interleaved={channel}-{};ssrc={METADATA_SSRC:08x}",
                        channel + 1
                    )
                    .try_into()?,
                );
                self.reply(&request, headers)?;
                self.metadata_channel = Some(channel);
                return Ok(true);
            }
            ensure!(
                self.metadata_channel
                    .is_none_or(|metadata| metadata.abs_diff(channel) > 1),
                "video channels overlap metadata channels"
            );
            self.video_channel = Some(channel);
        }
        ensure!(!self.playing, "RTSP session has already played");
        ensure!(
            self.requests.len() < REQUESTS_MAX,
            "too many pending RTSP requests"
        );
        if let Some(session) = &self.upstream_session {
            request
                .headers
                .insert(msg::HeaderName::SESSION, session.clone());
        }
        self.upstream
            .send(&msg::Message::Request(request.clone()), body)?;
        self.requests.push_back(request);
        Ok(true)
    }

    fn reply(&mut self, request: &msg::Request, mut headers: msg::Headers) -> anyhow::Result<()> {
        headers.insert(
            msg::HeaderName::CSEQ,
            request.headers.get("CSeq").context("missing CSeq")?.clone(),
        );
        headers.insert(msg::HeaderName::SESSION, SESSION_ID.try_into()?);
        self.client.send(
            &msg::Message::Response(msg::Response {
                status_code: msg::StatusCode::OK,
                reason_phrase: "OK".to_owned(),
                headers,
            }),
            &[],
        )
    }

    fn response(
        &mut self,
        message: msg::Message,
        body: &[u8],
        metadata: &Metadata,
    ) -> anyhow::Result<()> {
        let msg::Message::Response(mut response) = message else {
            return self.client.send(&message, body);
        };
        let request = self
            .requests
            .pop_front()
            .context("unsolicited upstream RTSP response")?;
        if let Some(session) = response.headers.get("Session") {
            self.upstream_session = Some(session.clone());
            response
                .headers
                .insert(msg::HeaderName::SESSION, SESSION_ID.try_into()?);
        }
        let mut amended_body = Vec::new();
        let mut body = body;
        if response.status_code.is_success() {
            if request.method == msg::Method::DESCRIBE {
                let track = format!(
                    "m=application 0 RTP/AVP 107\r\na=rtpmap:107 {}/90000\r\na=control:trackID=1\r\n",
                    metadata.encoding
                );
                amended_body.reserve_exact(body.len() + track.len());
                amended_body.extend_from_slice(body);
                amended_body.extend_from_slice(track.as_bytes());
                body = &amended_body;
                response.headers.insert(
                    "Content-Length".try_into()?,
                    body.len().to_string().try_into()?,
                );
            } else if request.method == msg::Method::PLAY {
                self.playing = true;
                if let Some(channel) = self.metadata_channel {
                    let url = request
                        .request_uri
                        .context("PLAY needs a presentation URL")?;
                    response.headers.append(
                        msg::HeaderName::RTP_INFO,
                        format!(
                            "url={}/trackID=1;seq=1;rtptime=0",
                            url.as_str().trim_end_matches('/')
                        )
                        .try_into()?,
                    );
                    self.progress = Some(Progress {
                        channel,
                        document: 0,
                        offset: 0,
                        sequence: 1,
                        ready_at: Instant::now(),
                    });
                }
            }
        }
        self.client.send(&msg::Message::Response(response), body)
    }
}

fn interleaved_channel(request: &msg::Request) -> anyhow::Result<u8> {
    let transport = request
        .headers
        .get("Transport")
        .context("SETUP needs Transport")?;
    let pair = transport
        .split(';')
        .find_map(|part| part.trim().strip_prefix("interleaved="))
        .context("metadata fixture requires TCP interleaved RTP")?;
    let (rtp, rtcp) = pair
        .split_once('-')
        .context("invalid interleaved channel pair")?;
    let rtp: u8 = rtp.parse()?;
    let rtcp: u8 = rtcp.parse()?;
    ensure!(
        rtp.checked_add(1) == Some(rtcp),
        "RTP and RTCP channels must be adjacent"
    );
    Ok(rtp)
}

struct Progress {
    channel: u8,
    document: usize,
    offset: usize,
    sequence: u16,
    ready_at: Instant,
}

impl Progress {
    const fn done(&self, metadata: &Metadata) -> bool {
        self.document == metadata.documents.len()
    }

    fn send_ready(
        &mut self,
        stream: &mut TcpStream,
        metadata: &Metadata,
        stop: &AtomicBool,
    ) -> anyhow::Result<()> {
        if self.done(metadata) || Instant::now() < self.ready_at {
            return Ok(());
        }
        let document = &metadata.documents[self.document];
        let timestamp = u32::try_from(self.document)? * METADATA_TIMESTAMP_STEP;
        let mut packet = [0; 4 + 12 + METADATA_PAYLOAD_MAX];
        for _ in 0..METADATA_PACKET_BATCH {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let end = (self.offset + METADATA_PAYLOAD_MAX).min(document.len());
            let final_fragment = end == document.len();
            let length = 12 + end - self.offset;
            packet[..2].copy_from_slice(&[b'$', self.channel]);
            packet[2..4].copy_from_slice(&u16::try_from(length)?.to_be_bytes());
            packet[4..6].copy_from_slice(&[0x80, 107 | if final_fragment { 0x80 } else { 0 }]);
            packet[6..8].copy_from_slice(&self.sequence.to_be_bytes());
            packet[8..12].copy_from_slice(&timestamp.to_be_bytes());
            packet[12..16].copy_from_slice(&METADATA_SSRC.to_be_bytes());
            packet[16..4 + length].copy_from_slice(&document[self.offset..end]);
            stream.write_all(&packet[..4 + length])?;
            self.sequence = self.sequence.wrapping_add(1);
            self.offset = end;
            if final_fragment {
                self.document += 1;
                self.offset = 0;
                self.ready_at = Instant::now() + METADATA_INTERVAL;
                break;
            }
        }
        Ok(())
    }
}
