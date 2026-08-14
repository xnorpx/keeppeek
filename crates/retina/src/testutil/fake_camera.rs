//! A deterministic RTSP camera for end-to-end client tests.

use crate::{client::core::RtspFramer, rtsp::msg};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::Bytes;
use std::{
    collections::VecDeque,
    fs::File,
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream, UdpSocket},
    num::NonZeroU32,
    path::Path,
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError},
    thread::{self, JoinHandle},
    time::Duration,
};
use url::Url;
#[cfg(windows)]
use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};

const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(10);
const READ_POLL_INTERVAL: Duration = Duration::from_millis(100);
const FAKE_SESSION_ID: &str = "fake-session";
const FAKE_SSRC: u32 = 0x0102_0304;
const RTP_CLOCK_RATE: u32 = 90_000;
const RTP_PAYLOAD_TYPE: u8 = 96;
const RTP_MAX_PAYLOAD_SIZE: u16 = 1_200;

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

const DEFAULT_DESCRIBE_BODY: &[u8] = b"v=0\r\n\
o=- 1 1 IN IP4 127.0.0.1\r\n\
s=Retina Fake Camera\r\n\
t=0 0\r\n\
a=control:*\r\n\
m=video 0 RTP/AVP 96\r\n\
a=rtpmap:96 H264/90000\r\n\
a=fmtp:96 profile-level-id=420029; packetization-mode=1; sprop-parameter-sets=Z00AKZpkA8ARPyzUBAQFAAADA+gAAOpgBA==,aO48gA==\r\n\
a=control:trackID=0\r\n";

/// Errors produced by [`FakeRtspCamera`].
#[derive(Debug, derive_more::Display)]
pub enum FakeRtspCameraError {
    /// The fake camera's local transport failed.
    #[display("fake RTSP camera I/O error: {_0}")]
    Io(io::Error),
    /// The client did not follow the supported RTSP interaction.
    #[display("fake RTSP camera protocol error: {_0}")]
    Protocol(String),
    /// The configured MP4 media source could not be streamed.
    #[display("fake RTSP camera MP4 source error: {_0}")]
    Mp4(String),
    /// The server worker panicked.
    #[display("fake RTSP camera worker panicked")]
    WorkerPanicked,
    /// The fake camera was dropped before its interaction completed.
    #[display("fake RTSP camera was stopped")]
    Stopped,
}

impl std::error::Error for FakeRtspCameraError {}

impl From<io::Error> for FakeRtspCameraError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// The client requests observed during a [`FakeRtspCamera`] interaction.
#[derive(Debug)]
pub struct FakeRtspCameraTranscript {
    requests: Vec<msg::Request>,
}

impl FakeRtspCameraTranscript {
    /// Returns the complete RTSP request heads received from the client.
    pub fn requests(&self) -> &[msg::Request] {
        &self.requests
    }
}

/// A deterministic RTSP camera that serves H.264 or H.265 profiles over TCP or UDP.
///
/// Each profile expects `DESCRIBE`, `SETUP`, and `PLAY` in that order. The
/// camera echoes request CSeq values, accepts TCP interleaved or UDP unicast
/// RTP transport, and emits its configured RTP packets. UDP sessions keep the
/// RTSP control connection open until the client closes it.
pub struct FakeRtspCamera {
    address: SocketAddr,
    stream_paths: Vec<Box<str>>,
    stop: Sender<()>,
    worker: Option<JoinHandle<Result<FakeRtspCameraTranscript, FakeRtspCameraError>>>,
    #[cfg(windows)]
    _timer_resolution: Option<WindowsTimerResolution>,
}

impl FakeRtspCamera {
    /// Starts a camera listener on an ephemeral loopback port.
    pub fn start() -> Result<Self, FakeRtspCameraError> {
        Self::start_with_sources(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            vec![NamedMediaSource {
                path: "stream".into(),
                source: MediaSource::default(),
            }],
        )
    }

    /// Starts a camera that holds its RTSP connection open after one RTP packet.
    pub fn start_stalled_after_first_packet() -> Result<Self, FakeRtspCameraError> {
        Self::start_with_sources(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            vec![NamedMediaSource {
                path: "stream".into(),
                source: MediaSource {
                    hold_after_media: true,
                    ..MediaSource::default()
                },
            }],
        )
    }

    /// Starts a camera that emits the first H.264 or H.265 track from an MP4 source.
    ///
    /// Samples are sent in file order without wall-clock pacing. Their RTP
    /// timestamps retain the MP4 track timing so client tests can verify media
    /// ordering without sleeping.
    pub fn from_mp4(path: impl AsRef<Path>) -> Result<Self, FakeRtspCameraError> {
        Self::from_mp4_on(SocketAddr::from(([127, 0, 0, 1], 0)), path)
    }

    /// Starts a camera at `address` that emits the first H.264 or H.265 MP4 track.
    pub fn from_mp4_on(
        address: SocketAddr,
        path: impl AsRef<Path>,
    ) -> Result<Self, FakeRtspCameraError> {
        Self::start_with_sources(
            address,
            vec![NamedMediaSource {
                path: "stream".into(),
                source: MediaSource::from_mp4(path.as_ref())?,
            }],
        )
    }

    /// Starts a camera with separate H.264 or H.265 MP4 sources for high and low profiles.
    ///
    /// The profiles are available through [`Self::high_resolution_url`] and
    /// [`Self::low_resolution_url`]. Each URL creates an independent RTSP
    /// session using its corresponding MP4 source.
    pub fn from_mp4_streams(
        high_resolution: impl AsRef<Path>,
        low_resolution: impl AsRef<Path>,
    ) -> Result<Self, FakeRtspCameraError> {
        Self::from_mp4_streams_on(
            SocketAddr::from(([127, 0, 0, 1], 0)),
            high_resolution,
            low_resolution,
        )
    }

    /// Starts a camera at `address` with separate H.264 or H.265 MP4 sources.
    pub fn from_mp4_streams_on(
        address: SocketAddr,
        high_resolution: impl AsRef<Path>,
        low_resolution: impl AsRef<Path>,
    ) -> Result<Self, FakeRtspCameraError> {
        Self::start_with_sources(
            address,
            vec![
                NamedMediaSource {
                    path: "high".into(),
                    source: MediaSource::from_mp4(high_resolution.as_ref())?,
                },
                NamedMediaSource {
                    path: "low".into(),
                    source: MediaSource::from_mp4(low_resolution.as_ref())?,
                },
            ],
        )
    }

    fn start_with_sources(
        address: SocketAddr,
        sources: Vec<NamedMediaSource>,
    ) -> Result<Self, FakeRtspCameraError> {
        #[cfg(windows)]
        let timer_resolution = WindowsTimerResolution::request(1);

        if sources.is_empty() {
            return Err(FakeRtspCameraError::Protocol(
                "fake camera needs at least one media source".to_string(),
            ));
        }
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let stream_paths = sources.iter().map(|source| source.path.clone()).collect();
        let (stop, stopped) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("fake-rtsp-camera".to_string())
            .spawn(move || serve(listener, stopped, sources))?;

        Ok(Self {
            address,
            stream_paths,
            stop,
            worker: Some(worker),
            #[cfg(windows)]
            _timer_resolution: timer_resolution,
        })
    }

    /// Returns the loopback address on which the fake camera accepts RTSP.
    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    /// Returns the first configured fake camera profile URL.
    pub fn url(&self) -> Url {
        self.profile_url(
            self.stream_paths
                .first()
                .expect("fake camera has at least one profile"),
        )
    }

    /// Returns the RTSP URL for a configured profile path.
    pub fn stream_url(&self, profile: &str) -> Option<Url> {
        self.stream_paths
            .iter()
            .any(|path| path.as_ref() == profile)
            .then(|| self.profile_url(profile))
    }

    /// Returns the high-resolution profile URL created by [`Self::from_mp4_streams`].
    pub fn high_resolution_url(&self) -> Url {
        self.stream_url("high")
            .expect("multi-profile fake camera has a high profile")
    }

    /// Returns the low-resolution profile URL created by [`Self::from_mp4_streams`].
    pub fn low_resolution_url(&self) -> Url {
        self.stream_url("low")
            .expect("multi-profile fake camera has a low profile")
    }

    /// Waits for the expected client interaction and returns its request transcript.
    pub fn finish(mut self) -> Result<FakeRtspCameraTranscript, FakeRtspCameraError> {
        let worker = self.worker.take().expect("fake camera worker is present");
        let _ = self.stop.send(());
        worker
            .join()
            .map_err(|_| FakeRtspCameraError::WorkerPanicked)?
    }

    fn profile_url(&self, profile: &str) -> Url {
        Url::parse(&format!("rtsp://{}/{profile}", self.address))
            .expect("loopback RTSP URL is valid")
    }
}

#[derive(Clone)]
struct MediaSource {
    describe_body: Bytes,
    rtp_packets: Vec<Bytes>,
    initial_sequence_number: u16,
    initial_timestamp: u32,
    hold_after_media: bool,
}

#[derive(Clone)]
struct NamedMediaSource {
    path: Box<str>,
    source: MediaSource,
}

impl Default for MediaSource {
    fn default() -> Self {
        Self {
            describe_body: Bytes::from_static(DEFAULT_DESCRIBE_BODY),
            rtp_packets: vec![Bytes::from_static(&[
                0x80, 0xe0, 0x00, 0x01, 0x00, 0x01, 0x5f, 0x90, 0x01, 0x02, 0x03, 0x04, 0x65, 0x88,
                0x84, 0x21,
            ])],
            initial_sequence_number: 1,
            initial_timestamp: 90_000,
            hold_after_media: false,
        }
    }
}

impl MediaSource {
    fn from_mp4(path: &Path) -> Result<Self, FakeRtspCameraError> {
        let reader = mp4::read_mp4(File::open(path)?)
            .map_err(|error| FakeRtspCameraError::Mp4(error.to_string()))?;
        let media_type = reader
            .tracks()
            .values()
            .filter_map(|track| {
                track
                    .media_type()
                    .ok()
                    .filter(|media_type| {
                        matches!(media_type, mp4::MediaType::H264 | mp4::MediaType::H265)
                    })
                    .map(|media_type| (track.track_id(), media_type))
            })
            .min_by_key(|(track_id, _)| *track_id)
            .map(|(_, media_type)| media_type)
            .ok_or_else(|| {
                FakeRtspCameraError::Mp4("MP4 source has no H.264 or H.265 video track".to_string())
            })?;
        match media_type {
            mp4::MediaType::H264 => Self::from_h264_mp4(path),
            mp4::MediaType::H265 => Self::from_h265_mp4(path),
            _ => unreachable!("video track selection only returns H.264 or H.265"),
        }
    }

    fn from_h264_mp4(path: &Path) -> Result<Self, FakeRtspCameraError> {
        let mut reader = mp4::read_mp4(File::open(path)?)
            .map_err(|error| FakeRtspCameraError::Mp4(error.to_string()))?;
        let mut h264_track = None;
        for track in reader.tracks().values() {
            if !matches!(track.media_type(), Ok(mp4::MediaType::H264)) {
                continue;
            }
            let avcc = track
                .trak
                .mdia
                .minf
                .stbl
                .stsd
                .avc1
                .as_ref()
                .ok_or_else(|| {
                    FakeRtspCameraError::Mp4("H.264 track has no avc1 sample entry".to_string())
                })?;
            let nal_length_size =
                avcc.avcc
                    .length_size_minus_one
                    .checked_add(1)
                    .ok_or_else(|| {
                        FakeRtspCameraError::Mp4("H.264 NAL length size overflowed".to_string())
                    })?;
            let sps = track
                .sequence_parameter_set()
                .map_err(|error| FakeRtspCameraError::Mp4(error.to_string()))?
                .to_vec();
            let pps = track
                .picture_parameter_set()
                .map_err(|error| FakeRtspCameraError::Mp4(error.to_string()))?
                .to_vec();
            h264_track = Some((
                track.track_id(),
                track.timescale(),
                track.width(),
                track.height(),
                nal_length_size,
                sps,
                pps,
            ));
            break;
        }
        let Some((track_id, timescale, width, height, nal_length_size, sps, pps)) = h264_track
        else {
            return Err(FakeRtspCameraError::Mp4(
                "MP4 source has no H.264 video track".to_string(),
            ));
        };
        if timescale == 0 {
            return Err(FakeRtspCameraError::Mp4(
                "H.264 track has a zero timescale".to_string(),
            ));
        }
        if !(1..=4).contains(&nal_length_size) {
            return Err(FakeRtspCameraError::Mp4(format!(
                "H.264 track uses unsupported NAL length size {nal_length_size}"
            )));
        }

        let sample_count = reader
            .sample_count(track_id)
            .map_err(|error| FakeRtspCameraError::Mp4(error.to_string()))?;
        if sample_count == 0 {
            return Err(FakeRtspCameraError::Mp4(
                "H.264 track has no samples".to_string(),
            ));
        }

        let clock_rate = NonZeroU32::new(RTP_CLOCK_RATE).expect("RTP clock rate is non-zero");
        let mut packetizer = crate::codec::h264::Packetizer::new(
            RTP_MAX_PAYLOAD_SIZE,
            0,
            1,
            RTP_PAYLOAD_TYPE,
            FAKE_SSRC,
        )
        .map_err(FakeRtspCameraError::Mp4)?;
        let mut first_timestamp = None;
        let mut first_packet = None;
        let mut rtp_packets = Vec::new();

        for sample_id in 1..=sample_count {
            let sample = reader
                .read_sample(track_id, sample_id)
                .map_err(|error| FakeRtspCameraError::Mp4(error.to_string()))?
                .ok_or_else(|| {
                    FakeRtspCameraError::Mp4(format!(
                        "H.264 sample {sample_id} is missing from the MP4 source"
                    ))
                })?;
            let rtp_timestamp = scale_timestamp(sample.start_time, timescale)?;
            let start = *first_timestamp.get_or_insert(rtp_timestamp);
            let timestamp = crate::Timestamp::new(i64::from(rtp_timestamp), clock_rate, start)
                .ok_or_else(|| {
                    FakeRtspCameraError::Mp4(
                        "MP4 sample timestamp underflowed the RTP timeline".to_string(),
                    )
                })?;
            let sample = normalize_avcc_sample(sample.bytes, nal_length_size)?;
            packetizer
                .push(timestamp, sample)
                .map_err(|error| FakeRtspCameraError::Mp4(error.to_string()))?;
            while let Some(packet) = packetizer.pull().map_err(FakeRtspCameraError::Mp4)? {
                if first_packet.is_none() {
                    let timestamp =
                        u32::try_from(packet.timestamp().timestamp()).map_err(|_| {
                            FakeRtspCameraError::Mp4(
                                "packetizer emitted an RTP timestamp outside u32".to_string(),
                            )
                        })?;
                    first_packet = Some((packet.sequence_number(), timestamp));
                }
                rtp_packets.push(Bytes::copy_from_slice(packet.raw()));
            }
        }

        let Some((initial_sequence_number, initial_timestamp)) = first_packet else {
            return Err(FakeRtspCameraError::Mp4(
                "H.264 MP4 source did not produce RTP packets".to_string(),
            ));
        };
        Ok(Self {
            describe_body: h264_describe_body(&sps, &pps, width, height)?,
            rtp_packets,
            initial_sequence_number,
            initial_timestamp,
            hold_after_media: false,
        })
    }

    #[cfg(feature = "h265")]
    fn from_h265_mp4(path: &Path) -> Result<Self, FakeRtspCameraError> {
        let mut reader = mp4::read_mp4(File::open(path)?)
            .map_err(|error| FakeRtspCameraError::Mp4(error.to_string()))?;
        let mut h265_track = None;
        for track in reader.tracks().values() {
            if !matches!(track.media_type(), Ok(mp4::MediaType::H265)) {
                continue;
            }
            let hev1 = track
                .trak
                .mdia
                .minf
                .stbl
                .stsd
                .hev1
                .as_ref()
                .or(track.trak.mdia.minf.stbl.stsd.hvc1.as_ref())
                .ok_or_else(|| {
                    FakeRtspCameraError::Mp4("H.265 track has no HEVC sample entry".to_string())
                })?;
            let configuration = hev1
                .hvcc
                .configuration()
                .map_err(|error| FakeRtspCameraError::Mp4(error.to_string()))?;
            let vps = configuration.vps.into_iter().next().ok_or_else(|| {
                FakeRtspCameraError::Mp4("H.265 hvcC record has no VPS".to_string())
            })?;
            let sps = configuration.sps.into_iter().next().ok_or_else(|| {
                FakeRtspCameraError::Mp4("H.265 hvcC record has no SPS".to_string())
            })?;
            let pps = configuration.pps.into_iter().next().ok_or_else(|| {
                FakeRtspCameraError::Mp4("H.265 hvcC record has no PPS".to_string())
            })?;
            h265_track = Some((
                track.track_id(),
                track.timescale(),
                track.width(),
                track.height(),
                configuration.nal_length_size,
                vps,
                sps,
                pps,
            ));
            break;
        }
        let Some((track_id, timescale, width, height, nal_length_size, vps, sps, pps)) = h265_track
        else {
            return Err(FakeRtspCameraError::Mp4(
                "MP4 source has no H.265 video track".to_string(),
            ));
        };
        if timescale == 0 {
            return Err(FakeRtspCameraError::Mp4(
                "H.265 track has a zero timescale".to_string(),
            ));
        }
        if !(1..=4).contains(&nal_length_size) {
            return Err(FakeRtspCameraError::Mp4(format!(
                "H.265 track uses unsupported NAL length size {nal_length_size}"
            )));
        }

        let sample_count = reader
            .sample_count(track_id)
            .map_err(|error| FakeRtspCameraError::Mp4(error.to_string()))?;
        if sample_count == 0 {
            return Err(FakeRtspCameraError::Mp4(
                "H.265 track has no samples".to_string(),
            ));
        }

        let mut initial_timestamp = None;
        let mut rtp_packets = Vec::new();
        let mut next_sequence_number = 1;
        for sample_id in 1..=sample_count {
            let sample = reader
                .read_sample(track_id, sample_id)
                .map_err(|error| FakeRtspCameraError::Mp4(error.to_string()))?
                .ok_or_else(|| {
                    FakeRtspCameraError::Mp4(format!(
                        "H.265 sample {sample_id} is missing from the MP4 source"
                    ))
                })?;
            let timestamp = scale_timestamp(sample.start_time, timescale)?;
            let packet_count = rtp_packets.len();
            packetize_h265_sample(
                &sample.bytes,
                nal_length_size,
                timestamp,
                &mut next_sequence_number,
                &mut rtp_packets,
            )?;
            if packet_count != rtp_packets.len() && initial_timestamp.is_none() {
                initial_timestamp = Some(timestamp);
            }
        }

        let Some(initial_timestamp) = initial_timestamp else {
            return Err(FakeRtspCameraError::Mp4(
                "H.265 MP4 source did not produce RTP packets".to_string(),
            ));
        };
        Ok(Self {
            describe_body: h265_describe_body(&vps, &sps, &pps, width, height),
            rtp_packets,
            initial_sequence_number: 1,
            initial_timestamp,
            hold_after_media: false,
        })
    }

    #[cfg(not(feature = "h265"))]
    fn from_h265_mp4(_path: &Path) -> Result<Self, FakeRtspCameraError> {
        Err(FakeRtspCameraError::Mp4(
            "H.265 MP4 sources require Retina's h265 feature".to_string(),
        ))
    }
}

impl Drop for FakeRtspCamera {
    fn drop(&mut self) {
        let Some(worker) = self.worker.take() else {
            return;
        };
        let _ = self.stop.send(());
        let _ = worker.join();
    }
}

struct Connection<'a> {
    stream: TcpStream,
    framer: RtspFramer,
    messages: VecDeque<msg::Message>,
    stop: &'a Receiver<()>,
}

impl Connection<'_> {
    fn next_request(&mut self) -> Result<msg::Request, FakeRtspCameraError> {
        let mut buffer = [0_u8; 16 * 1024];

        loop {
            if let Some(message) = self.messages.pop_front() {
                return match message {
                    msg::Message::Request(request) => Ok(request),
                    message => Err(FakeRtspCameraError::Protocol(format!(
                        "expected RTSP request, got {message:?}"
                    ))),
                };
            }

            match self.stream.read(&mut buffer) {
                Ok(0) => {
                    return Err(FakeRtspCameraError::Protocol(
                        "client closed the TCP connection before completing RTSP setup".to_string(),
                    ));
                }
                Ok(read) => {
                    let messages = self
                        .framer
                        .push(&buffer[..read])
                        .map_err(|error| FakeRtspCameraError::Protocol(error.to_string()))?;
                    self.messages
                        .extend(messages.into_iter().map(|message| message.message));
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    if stopped(self.stop) {
                        return Err(FakeRtspCameraError::Stopped);
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn wait_for_client_close(&mut self) -> Result<(), FakeRtspCameraError> {
        let mut buffer = [0_u8; 16 * 1024];

        loop {
            match self.stream.read(&mut buffer) {
                Ok(0) => return Ok(()),
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    if stopped(self.stop) {
                        return Err(FakeRtspCameraError::Stopped);
                    }
                }
                Err(error) if client_disconnected(&error) => {
                    return Ok(());
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

fn client_disconnected(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::NotConnected
    )
}

fn serve(
    listener: TcpListener,
    stop: Receiver<()>,
    sources: Vec<NamedMediaSource>,
) -> Result<FakeRtspCameraTranscript, FakeRtspCameraError> {
    let mut transcript = FakeRtspCameraTranscript {
        requests: Vec::new(),
    };

    loop {
        let stream = match accept_one(&listener, &stop) {
            Ok(stream) => stream,
            Err(FakeRtspCameraError::Stopped) => return Ok(transcript),
            Err(error) => return Err(error),
        };
        stream.set_read_timeout(Some(READ_POLL_INTERVAL))?;
        stream.set_write_timeout(Some(READ_POLL_INTERVAL))?;
        let mut connection = Connection {
            stream,
            framer: RtspFramer::default(),
            messages: VecDeque::new(),
            stop: &stop,
        };
        let describe = expect_request(&mut connection, msg::Method::DESCRIBE, &mut transcript)?;
        expect_header(&describe, "Accept", "application/sdp")?;
        let presentation_url = request_url(&describe)?;
        let source = source_for(&sources, &presentation_url)?;
        match serve_presentation(
            &mut connection,
            describe,
            presentation_url,
            source,
            &mut transcript,
        ) {
            Ok(()) => {}
            Err(FakeRtspCameraError::Stopped) => return Ok(transcript),
            Err(error) => return Err(error),
        }
    }
}

fn source_for(
    sources: &[NamedMediaSource],
    presentation_url: &Url,
) -> Result<MediaSource, FakeRtspCameraError> {
    let requested_path = presentation_url.path().trim_matches('/');
    sources
        .iter()
        .find(|source| source.path.as_ref() == requested_path)
        .map(|source| source.source.clone())
        .ok_or_else(|| {
            FakeRtspCameraError::Protocol(format!("no fake camera profile for {presentation_url}"))
        })
}

fn serve_presentation(
    connection: &mut Connection<'_>,
    describe: msg::Request,
    presentation_url: Url,
    source: MediaSource,
    transcript: &mut FakeRtspCameraTranscript,
) -> Result<(), FakeRtspCameraError> {
    let stream_url = track_url(&presentation_url)?;
    send_describe(
        &mut connection.stream,
        &cseq(&describe)?,
        &source.describe_body,
    )?;

    let setup = expect_request(connection, msg::Method::SETUP, transcript)?;
    if setup.request_uri.as_ref() != Some(&stream_url) {
        return Err(FakeRtspCameraError::Protocol(format!(
            "expected SETUP for {stream_url}, got {:?}",
            setup.request_uri
        )));
    }
    let transport = setup_transport(&setup, &connection.stream)?;
    send_setup(&mut connection.stream, &cseq(&setup)?, &transport)?;

    let play = expect_request(connection, msg::Method::PLAY, transcript)?;
    if play.request_uri.as_ref() != Some(&presentation_url) {
        return Err(FakeRtspCameraError::Protocol(format!(
            "expected PLAY for {presentation_url}, got {:?}",
            play.request_uri
        )));
    }
    expect_header(&play, "Session", FAKE_SESSION_ID)?;
    send_play(&mut connection.stream, &cseq(&play)?, &stream_url, &source)?;
    send_rtp_packets(&mut connection.stream, &transport, &source.rtp_packets)?;
    if source.hold_after_media {
        loop {
            if stopped(connection.stop) {
                return Err(FakeRtspCameraError::Stopped);
            }
            thread::sleep(READ_POLL_INTERVAL);
        }
    }
    if matches!(&transport, SessionTransport::Udp { .. }) {
        connection.wait_for_client_close()?;
    }
    Ok(())
}

fn accept_one(
    listener: &TcpListener,
    stop: &Receiver<()>,
) -> Result<TcpStream, FakeRtspCameraError> {
    loop {
        match listener.accept() {
            Ok((stream, _)) => return Ok(stream),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                match stop.recv_timeout(ACCEPT_POLL_INTERVAL) {
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                        return Err(FakeRtspCameraError::Stopped);
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn stopped(stop: &Receiver<()>) -> bool {
    matches!(stop.try_recv(), Ok(()) | Err(TryRecvError::Disconnected))
}

fn expect_request(
    connection: &mut Connection,
    method: msg::Method,
    transcript: &mut FakeRtspCameraTranscript,
) -> Result<msg::Request, FakeRtspCameraError> {
    loop {
        let request = connection.next_request()?;
        if request.method == msg::Method::OPTIONS {
            send_options(&mut connection.stream, &cseq(&request)?)?;
            transcript.requests.push(request);
            continue;
        }
        if request.method != method {
            return Err(FakeRtspCameraError::Protocol(format!(
                "expected {method} request, got {}",
                request.method
            )));
        }
        transcript.requests.push(request.clone());
        return Ok(request);
    }
}

fn request_url(request: &msg::Request) -> Result<Url, FakeRtspCameraError> {
    request
        .request_uri
        .clone()
        .ok_or_else(|| FakeRtspCameraError::Protocol("RTSP request URI must not be *".to_string()))
}

fn track_url(presentation_url: &Url) -> Result<Url, FakeRtspCameraError> {
    Url::parse(&format!(
        "{}/trackID=0",
        presentation_url.as_str().trim_end_matches('/')
    ))
    .map_err(|error| FakeRtspCameraError::Protocol(error.to_string()))
}

fn cseq(request: &msg::Request) -> Result<String, FakeRtspCameraError> {
    request
        .headers
        .get("CSeq")
        .map(ToString::to_string)
        .ok_or_else(|| FakeRtspCameraError::Protocol("request has no CSeq header".to_string()))
}

fn expect_header(
    request: &msg::Request,
    name: &str,
    expected: &str,
) -> Result<(), FakeRtspCameraError> {
    match request.headers.get(name).map(ToString::to_string) {
        Some(value) if value == expected => Ok(()),
        Some(value) => Err(FakeRtspCameraError::Protocol(format!(
            "expected {name}: {expected}, got {value}"
        ))),
        None => Err(FakeRtspCameraError::Protocol(format!(
            "request has no {name} header"
        ))),
    }
}

enum SessionTransport {
    Tcp {
        channel: u8,
    },
    Udp {
        rtp: UdpSocket,
        _rtcp: UdpSocket,
        client_rtp: SocketAddr,
    },
}

fn setup_transport(
    request: &msg::Request,
    stream: &TcpStream,
) -> Result<SessionTransport, FakeRtspCameraError> {
    let transport = request.headers.get("Transport").ok_or_else(|| {
        FakeRtspCameraError::Protocol("SETUP request has no Transport header".to_string())
    })?;
    if let Some(channels) = transport
        .split(';')
        .find_map(|part| part.trim().strip_prefix("interleaved="))
    {
        let (rtp, rtcp) = channels.split_once('-').ok_or_else(|| {
            FakeRtspCameraError::Protocol("invalid interleaved channel range".to_string())
        })?;
        let rtp = rtp.parse::<u8>().map_err(|error| {
            FakeRtspCameraError::Protocol(format!("invalid RTP interleaved channel: {error}"))
        })?;
        let rtcp = rtcp.parse::<u8>().map_err(|error| {
            FakeRtspCameraError::Protocol(format!("invalid RTCP interleaved channel: {error}"))
        })?;
        if rtp.checked_add(1) != Some(rtcp) {
            return Err(FakeRtspCameraError::Protocol(format!(
                "expected adjacent interleaved channels, got {rtp}-{rtcp}"
            )));
        }
        return Ok(SessionTransport::Tcp { channel: rtp });
    }

    let client_ports = transport
        .split(';')
        .find_map(|part| part.trim().strip_prefix("client_port="))
        .ok_or_else(|| {
            FakeRtspCameraError::Protocol(
                "SETUP request has neither interleaved nor client_port transport".to_string(),
            )
        })?;
    let (client_rtp_port, client_rtcp_port) = parse_port_pair(client_ports, "client")?;
    let peer_ip = stream.peer_addr()?.ip();
    let local_ip = stream.local_addr()?.ip();
    let (rtp, rtcp) = bind_udp_pair(local_ip)?;
    let client_rtp = SocketAddr::new(peer_ip, client_rtp_port);
    let client_rtcp = SocketAddr::new(peer_ip, client_rtcp_port);
    let _ = client_rtcp;
    Ok(SessionTransport::Udp {
        rtp,
        _rtcp: rtcp,
        client_rtp,
    })
}

fn parse_port_pair(ports: &str, side: &str) -> Result<(u16, u16), FakeRtspCameraError> {
    let (rtp, rtcp) = ports
        .split_once('-')
        .ok_or_else(|| FakeRtspCameraError::Protocol(format!("invalid {side}_port range")))?;
    let rtp = rtp.parse::<u16>().map_err(|error| {
        FakeRtspCameraError::Protocol(format!("invalid {side} RTP port: {error}"))
    })?;
    let rtcp = rtcp.parse::<u16>().map_err(|error| {
        FakeRtspCameraError::Protocol(format!("invalid {side} RTCP port: {error}"))
    })?;
    if rtp.checked_add(1) != Some(rtcp) {
        return Err(FakeRtspCameraError::Protocol(format!(
            "expected adjacent {side} RTP/RTCP ports, got {rtp}-{rtcp}"
        )));
    }
    Ok((rtp, rtcp))
}

fn bind_udp_pair(
    local_ip: std::net::IpAddr,
) -> Result<(UdpSocket, UdpSocket), FakeRtspCameraError> {
    for _ in 0..100 {
        let rtp = UdpSocket::bind(SocketAddr::new(local_ip, 0))?;
        let rtp_port = rtp.local_addr()?.port();
        let Some(rtcp_port) = rtp_port.checked_add(1) else {
            continue;
        };
        if let Ok(rtcp) = UdpSocket::bind(SocketAddr::new(local_ip, rtcp_port)) {
            return Ok((rtp, rtcp));
        }
    }
    Err(FakeRtspCameraError::Protocol(
        "unable to allocate an adjacent UDP RTP/RTCP port pair".to_string(),
    ))
}

fn send_describe(
    stream: &mut TcpStream,
    cseq: &str,
    body: &Bytes,
) -> Result<(), FakeRtspCameraError> {
    let mut headers = response_headers(cseq)?;
    headers.insert(
        msg::HeaderName::CONTENT_TYPE,
        header_value("application/sdp")?,
    );
    headers.insert(
        header_name("Content-Length")?,
        header_value(body.len().to_string())?,
    );
    send_response(stream, headers, body.clone())
}

fn send_options(stream: &mut TcpStream, cseq: &str) -> Result<(), FakeRtspCameraError> {
    let mut headers = response_headers(cseq)?;
    headers.insert(
        msg::HeaderName::PUBLIC,
        header_value("OPTIONS, DESCRIBE, SETUP, PLAY")?,
    );
    send_response(stream, headers, Bytes::new())
}

fn send_setup(
    stream: &mut TcpStream,
    cseq: &str,
    transport: &SessionTransport,
) -> Result<(), FakeRtspCameraError> {
    let mut headers = response_headers(cseq)?;
    headers.insert(msg::HeaderName::SESSION, header_value(FAKE_SESSION_ID)?);
    let transport_header = match transport {
        SessionTransport::Tcp { channel } => format!(
            "RTP/AVP/TCP;unicast;interleaved={channel}-{};ssrc={FAKE_SSRC:08x}",
            channel + 1
        ),
        SessionTransport::Udp {
            rtp, client_rtp, ..
        } => {
            let server_port = rtp.local_addr()?.port();
            format!(
                "RTP/AVP/UDP;unicast;client_port={}-{};server_port={server_port}-{};source={};ssrc={FAKE_SSRC:08x}",
                client_rtp.port(),
                client_rtp.port() + 1,
                server_port + 1,
                rtp.local_addr()?.ip(),
            )
        }
    };
    headers.insert(msg::HeaderName::TRANSPORT, header_value(transport_header)?);
    send_response(stream, headers, Bytes::new())
}

fn send_play(
    stream: &mut TcpStream,
    cseq: &str,
    stream_url: &Url,
    source: &MediaSource,
) -> Result<(), FakeRtspCameraError> {
    let mut headers = response_headers(cseq)?;
    headers.insert(msg::HeaderName::SESSION, header_value(FAKE_SESSION_ID)?);
    headers.insert(
        msg::HeaderName::RTP_INFO,
        header_value(format!(
            "url={stream_url};seq={};rtptime={}",
            source.initial_sequence_number, source.initial_timestamp
        ))?,
    );
    send_response(stream, headers, Bytes::new())
}

fn send_rtp_packets(
    stream: &mut TcpStream,
    transport: &SessionTransport,
    packets: &[Bytes],
) -> Result<(), FakeRtspCameraError> {
    let accepts_client_close = matches!(transport, SessionTransport::Tcp { .. });
    for packet in packets {
        let result = match transport {
            SessionTransport::Tcp { channel } => msg::OwnedMessage::Data {
                channel_id: *channel,
                body: packet.clone(),
            }
            .write(stream),
            SessionTransport::Udp {
                rtp, client_rtp, ..
            } => rtp.send_to(packet, client_rtp).map(|_| ()),
        };
        if let Err(error) = result {
            if accepts_client_close && client_disconnected(&error) {
                return Ok(());
            }
            return Err(error.into());
        }
    }
    match stream.flush() {
        Ok(()) => Ok(()),
        Err(error) if accepts_client_close && client_disconnected(&error) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn h264_describe_body(
    sps: &[u8],
    pps: &[u8],
    width: u16,
    height: u16,
) -> Result<Bytes, FakeRtspCameraError> {
    let [_, profile, compatibility, level, ..] = sps else {
        return Err(FakeRtspCameraError::Mp4(
            "H.264 SPS is too short to derive profile-level-id".to_string(),
        ));
    };
    Ok(Bytes::from(format!(
        "v=0\r\n\
o=- 1 1 IN IP4 127.0.0.1\r\n\
s=Retina Fake Camera\r\n\
t=0 0\r\n\
a=control:*\r\n\
m=video 0 RTP/AVP {RTP_PAYLOAD_TYPE}\r\n\
a=rtpmap:{RTP_PAYLOAD_TYPE} H264/{RTP_CLOCK_RATE}\r\n\
a=x-dimensions:{width},{height}\r\n\
a=fmtp:{RTP_PAYLOAD_TYPE} profile-level-id={profile:02x}{compatibility:02x}{level:02x}; packetization-mode=1; sprop-parameter-sets={},{}\r\n\
a=control:trackID=0\r\n",
        STANDARD.encode(sps),
        STANDARD.encode(pps),
    )))
}

#[cfg(feature = "h265")]
fn h265_describe_body(vps: &[u8], sps: &[u8], pps: &[u8], width: u16, height: u16) -> Bytes {
    Bytes::from(format!(
        "v=0\r\n\
o=- 1 1 IN IP4 127.0.0.1\r\n\
s=Retina Fake Camera\r\n\
t=0 0\r\n\
a=control:*\r\n\
m=video 0 RTP/AVP {RTP_PAYLOAD_TYPE}\r\n\
a=rtpmap:{RTP_PAYLOAD_TYPE} H265/{RTP_CLOCK_RATE}\r\n\
a=x-dimensions:{width},{height}\r\n\
a=fmtp:{RTP_PAYLOAD_TYPE} tx-mode=SRST; sprop-vps={}; sprop-sps={}; sprop-pps={}\r\n\
a=control:trackID=0\r\n",
        STANDARD.encode(vps),
        STANDARD.encode(sps),
        STANDARD.encode(pps),
    ))
}

fn normalize_avcc_sample(sample: Bytes, nal_length_size: u8) -> Result<Bytes, FakeRtspCameraError> {
    let mut input = sample.as_ref();
    let mut output = Vec::with_capacity(sample.len());
    while !input.is_empty() {
        if input.len() < usize::from(nal_length_size) {
            return Err(FakeRtspCameraError::Mp4(
                "H.264 sample ends inside an AVCC NAL length".to_string(),
            ));
        }
        let (length, remaining) = input.split_at(usize::from(nal_length_size));
        let nal_length = length
            .iter()
            .fold(0_usize, |length, byte| (length << 8) | usize::from(*byte));
        if nal_length == 0 || remaining.len() < nal_length {
            return Err(FakeRtspCameraError::Mp4(
                "H.264 sample has an invalid AVCC NAL length".to_string(),
            ));
        }
        output.extend_from_slice(
            &u32::try_from(nal_length)
                .map_err(|_| {
                    FakeRtspCameraError::Mp4(
                        "H.264 NAL exceeds the RTP packetizer limit".to_string(),
                    )
                })?
                .to_be_bytes(),
        );
        output.extend_from_slice(&remaining[..nal_length]);
        input = &remaining[nal_length..];
    }
    Ok(Bytes::from(output))
}

#[cfg(feature = "h265")]
fn packetize_h265_sample(
    sample: &[u8],
    nal_length_size: u8,
    timestamp: u32,
    next_sequence_number: &mut u16,
    packets: &mut Vec<Bytes>,
) -> Result<(), FakeRtspCameraError> {
    let nals = length_prefixed_nals(sample, nal_length_size, "H.265")?;
    for (index, nal) in nals.iter().enumerate() {
        packetize_h265_nal(
            nal,
            index + 1 == nals.len(),
            timestamp,
            next_sequence_number,
            packets,
        )?;
    }
    Ok(())
}

#[cfg(feature = "h265")]
fn length_prefixed_nals<'a>(
    sample: &'a [u8],
    nal_length_size: u8,
    codec: &str,
) -> Result<Vec<&'a [u8]>, FakeRtspCameraError> {
    let mut input = sample;
    let mut nals = Vec::new();
    while !input.is_empty() {
        if input.len() < usize::from(nal_length_size) {
            return Err(FakeRtspCameraError::Mp4(format!(
                "{codec} sample ends inside an AVCC NAL length"
            )));
        }
        let (length, remaining) = input.split_at(usize::from(nal_length_size));
        let nal_length = length
            .iter()
            .fold(0_usize, |length, byte| (length << 8) | usize::from(*byte));
        if nal_length == 0 || remaining.len() < nal_length {
            return Err(FakeRtspCameraError::Mp4(format!(
                "{codec} sample has an invalid AVCC NAL length"
            )));
        }
        nals.push(&remaining[..nal_length]);
        input = &remaining[nal_length..];
    }
    if nals.is_empty() {
        return Err(FakeRtspCameraError::Mp4(format!(
            "{codec} sample has no NAL units"
        )));
    }
    Ok(nals)
}

#[cfg(feature = "h265")]
fn packetize_h265_nal(
    nal: &[u8],
    mark: bool,
    timestamp: u32,
    next_sequence_number: &mut u16,
    packets: &mut Vec<Bytes>,
) -> Result<(), FakeRtspCameraError> {
    let Some((&first, rest)) = nal.split_first() else {
        return Err(FakeRtspCameraError::Mp4(
            "H.265 sample has an empty NAL unit".to_string(),
        ));
    };
    let Some((&second, payload)) = rest.split_first() else {
        return Err(FakeRtspCameraError::Mp4(
            "H.265 NAL is missing its second header byte".to_string(),
        ));
    };
    if (first & 0x80) != 0 || (second & 0x07) == 0 {
        return Err(FakeRtspCameraError::Mp4(
            "H.265 NAL has an invalid header".to_string(),
        ));
    }
    let nal_type = (first >> 1) & 0x3f;
    if nal_type >= 48 {
        return Err(FakeRtspCameraError::Mp4(
            "H.265 MP4 sample contains an RTP packetization NAL".to_string(),
        ));
    }
    if nal.len() <= usize::from(RTP_MAX_PAYLOAD_SIZE) {
        push_rtp_packet(nal, mark, timestamp, next_sequence_number, packets);
        return Ok(());
    }

    let fragment_capacity = usize::from(RTP_MAX_PAYLOAD_SIZE) - 3;
    let payload_header = [(first & 0x81) | (49 << 1), second];
    let mut offset = 0;
    while offset < payload.len() {
        let end = (offset + fragment_capacity).min(payload.len());
        let start = offset == 0;
        let final_fragment = end == payload.len();
        let mut fragment = Vec::with_capacity(3 + end - offset);
        fragment.extend_from_slice(&payload_header);
        fragment.push((u8::from(start) << 7) | (u8::from(final_fragment) << 6) | nal_type);
        fragment.extend_from_slice(&payload[offset..end]);
        push_rtp_packet(
            &fragment,
            mark && final_fragment,
            timestamp,
            next_sequence_number,
            packets,
        );
        offset = end;
    }
    Ok(())
}

#[cfg(feature = "h265")]
fn push_rtp_packet(
    payload: &[u8],
    mark: bool,
    timestamp: u32,
    next_sequence_number: &mut u16,
    packets: &mut Vec<Bytes>,
) {
    let mut packet = Vec::with_capacity(12 + payload.len());
    packet.extend_from_slice(&[
        0x80,
        (u8::from(mark) << 7) | RTP_PAYLOAD_TYPE,
        (*next_sequence_number >> 8) as u8,
        *next_sequence_number as u8,
    ]);
    packet.extend_from_slice(&timestamp.to_be_bytes());
    packet.extend_from_slice(&FAKE_SSRC.to_be_bytes());
    packet.extend_from_slice(payload);
    *next_sequence_number = next_sequence_number.wrapping_add(1);
    packets.push(Bytes::from(packet));
}

fn scale_timestamp(start_time: u64, timescale: u32) -> Result<u32, FakeRtspCameraError> {
    let ticks = start_time
        .checked_mul(u64::from(RTP_CLOCK_RATE))
        .ok_or_else(|| FakeRtspCameraError::Mp4("MP4 timestamp overflowed".to_string()))?
        / u64::from(timescale);
    u32::try_from(ticks)
        .map_err(|_| FakeRtspCameraError::Mp4("MP4 timestamp overflowed".to_string()))
}

fn response_headers(cseq: &str) -> Result<msg::Headers, FakeRtspCameraError> {
    Ok([(msg::HeaderName::CSEQ, header_value(cseq)?)].into())
}

fn header_name(name: &str) -> Result<msg::HeaderName, FakeRtspCameraError> {
    msg::HeaderName::try_from(name)
        .map_err(|error| FakeRtspCameraError::Protocol(error.to_string()))
}

fn header_value(value: impl Into<String>) -> Result<msg::HeaderValue, FakeRtspCameraError> {
    msg::HeaderValue::try_from(value.into())
        .map_err(|error| FakeRtspCameraError::Protocol(error.to_string()))
}

fn send_response(
    stream: &mut TcpStream,
    headers: msg::Headers,
    body: Bytes,
) -> Result<(), FakeRtspCameraError> {
    msg::OwnedMessage::Response {
        head: msg::Response {
            status_code: msg::StatusCode::OK,
            reason_phrase: "OK".to_string(),
            headers,
        },
        body,
    }
    .write(stream)?;
    stream.flush()?;
    Ok(())
}
