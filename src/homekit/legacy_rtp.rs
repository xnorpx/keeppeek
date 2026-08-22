use hap_video::{
    LegacyH264Level, LegacyH264Profile, LegacyRtpAddress, LegacySessionId, LegacySrtpParameters,
    LegacyStreamCommand, LegacyVideoParameters, SelectedStreamConfiguration, SetupEndpointsRequest,
    SetupEndpointsResponse, SrtcpSession, SrtpSession, encode_setup_endpoints_response,
};
use std::{
    collections::HashMap,
    fmt,
    io::ErrorKind,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

const DEFAULT_RTP_PACKET_SIZE: u16 = 1_378;
const MIN_RTP_PACKET_SIZE: u16 = 188;
const MAX_RTP_PACKET_SIZE: u16 = 1_500;
const RELAY_READ_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Clone)]
pub(super) enum FfmpegInput {
    File(PathBuf),
    Rtsp {
        url: String,
        transport: &'static str,
    },
}

impl fmt::Debug for FfmpegInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(path) => f.debug_tuple("File").field(path).finish(),
            Self::Rtsp { transport, .. } => f
                .debug_struct("Rtsp")
                .field("url", &"<redacted>")
                .field("transport", transport)
                .finish(),
        }
    }
}

pub(super) struct LegacyRtpManager {
    ffmpeg: PathBuf,
    input: Option<FfmpegInput>,
    force_hevc: bool,
    sessions: HashMap<u64, LegacyRtpSession>,
}

impl LegacyRtpManager {
    pub(super) fn new(ffmpeg: PathBuf, input: Option<FfmpegInput>, force_hevc: bool) -> Self {
        Self {
            ffmpeg,
            input,
            force_hevc,
            sessions: HashMap::new(),
        }
    }

    pub(super) fn prepare(
        &mut self,
        setup_iid: u64,
        hap_local_ip: IpAddr,
        request: SetupEndpointsRequest,
    ) -> std::io::Result<Vec<u8>> {
        let media_ip = routed_local_ip(
            SocketAddr::new(request.controller.ip, request.controller.video_port),
            hap_local_ip,
        );
        let video_return = bind_return_socket(media_ip)?;
        let audio_return = bind_return_socket(media_ip)?;
        let video_ssrc = nonzero_random_u32();
        let audio_ssrc = nonzero_random_u32();
        // The request carries the controller-to-accessory key; the accessory owns
        // a separate key for its own direction rather than reusing that one.
        let video_srtp = random_srtp_parameters();
        let audio_srtp = random_srtp_parameters();
        let response = SetupEndpointsResponse {
            session_id: request.session_id,
            accessory: LegacyRtpAddress {
                ip: media_ip,
                video_port: video_return.local_addr()?.port(),
                audio_port: audio_return.local_addr()?.port(),
            },
            video_srtp: video_srtp.clone(),
            audio_srtp,
            video_ssrc,
            audio_ssrc,
        };
        self.sessions.insert(
            setup_iid,
            LegacyRtpSession {
                request,
                video_ssrc,
                video_srtp,
                video_return,
                _audio_return: audio_return,
                selected_video: None,
                child: None,
                relay: None,
            },
        );
        tracing::info!(%hap_local_ip, %media_ip, "HomeKit legacy media route selected");
        Ok(encode_setup_endpoints_response(&response))
    }

    pub(super) fn apply_selected(
        &mut self,
        selected_iid: u64,
        configuration: SelectedStreamConfiguration,
    ) -> anyhow::Result<bool> {
        let setup_iid = setup_iid_for_selected(selected_iid)
            .ok_or_else(|| anyhow::anyhow!("unknown Selected RTP Stream IID {selected_iid}"))?;
        if configuration.command == LegacyStreamCommand::End {
            let Some(mut session) = self.sessions.remove(&setup_iid) else {
                anyhow::bail!("legacy RTP session is not prepared")
            };
            ensure_session_id(&session, configuration.session_id)?;
            session.stop();
            return Ok(false);
        }
        let session = self
            .sessions
            .get_mut(&setup_iid)
            .ok_or_else(|| anyhow::anyhow!("legacy RTP session is not prepared"))?;
        ensure_session_id(session, configuration.session_id)?;
        if let Some(video) = configuration.video {
            session.selected_video = Some(video);
        }
        match configuration.command {
            LegacyStreamCommand::Start
            | LegacyStreamCommand::Resume
            | LegacyStreamCommand::Reconfigure => {
                let input = self.input.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("camera has no FFmpeg-compatible video source")
                })?;
                session.start(&self.ffmpeg, input, self.force_hevc)?;
                Ok(true)
            }
            LegacyStreamCommand::Suspend => {
                session.stop();
                Ok(true)
            }
            LegacyStreamCommand::End => unreachable!("end command handled before session lookup"),
        }
    }
}

struct LegacyRtpSession {
    request: SetupEndpointsRequest,
    video_ssrc: u32,
    video_srtp: LegacySrtpParameters,
    video_return: UdpSocket,
    _audio_return: UdpSocket,
    selected_video: Option<LegacyVideoParameters>,
    child: Option<Child>,
    relay: Option<SrtpRelay>,
}

impl LegacyRtpSession {
    fn start(
        &mut self,
        ffmpeg: &PathBuf,
        input: &FfmpegInput,
        force_hevc: bool,
    ) -> anyhow::Result<()> {
        self.stop();
        let video = self
            .selected_video
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Selected RTP Stream has no video configuration"))?;
        let relay_address = self.video_return.local_addr()?;
        let relay = SrtpRelay::start(
            self.video_return.try_clone()?,
            SocketAddr::new(
                self.request.controller.ip,
                self.request.controller.video_port,
            ),
            self.video_srtp.clone(),
            video.payload_type,
        )?;
        let args = ffmpeg_args(input, video, self.video_ssrc, relay_address, force_hevc);
        let child = match Command::new(ffmpeg)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                relay.stop();
                return Err(anyhow::anyhow!(
                    "unable to start {}: {error}",
                    ffmpeg.display()
                ));
            }
        };
        tracing::info!(
            session_id = ?self.request.session_id,
            controller = %self.request.controller.ip,
            port = self.request.controller.video_port,
            relay_bound = ?self.video_return.local_addr().ok(),
            ffmpeg_target = %relay_address,
            width = video.width,
            height = video.height,
            frame_rate = video.frame_rate,
            profile = ?video.profile,
            level = ?video.level,
            bitrate_kbps = video.maximum_bitrate_kbps,
            payload_type = video.payload_type,
            codec = video.codec,
            force_hevc,
            ssrc = self.video_ssrc,
            controller_ssrc = video.ssrc,
            "HomeKit legacy RTP stream started"
        );
        self.child = Some(child);
        self.relay = Some(relay);
        Ok(())
    }

    fn stop(&mut self) {
        let had_child = self.child.is_some();
        if let Some(mut child) = self.child.take() {
            if matches!(child.try_wait(), Ok(None)) {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        if let Some(relay) = self.relay.take() {
            relay.stop();
        }
        if !had_child {
            return;
        }
        tracing::info!(
            session_id = ?self.request.session_id,
            "HomeKit legacy H.264 stream stopped"
        );
    }
}

struct SrtpRelay {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

impl SrtpRelay {
    fn start(
        socket: UdpSocket,
        destination: SocketAddr,
        parameters: LegacySrtpParameters,
        payload_type: u8,
    ) -> std::io::Result<Self> {
        socket.set_read_timeout(Some(RELAY_READ_TIMEOUT))?;
        let producer_ip = socket.local_addr()?.ip();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let handle = std::thread::Builder::new()
            .name("homekit-srtp-relay".to_owned())
            .spawn(move || {
                let mut srtp = SrtpSession::new(&parameters.master_key, &parameters.master_salt);
                let mut srtcp = SrtcpSession::new(&parameters.master_key, &parameters.master_salt);
                let mut buffer = [0_u8; 2_048];
                let mut rtp_packets = 0_u64;
                let mut rtcp_packets = 0_u64;
                let mut bytes = 0_u64;
                while !worker_stop.load(Ordering::Acquire) {
                    let (length, source) = match socket.recv_from(&mut buffer) {
                        Ok(received) => received,
                        Err(error)
                            if matches!(
                                error.kind(),
                                ErrorKind::WouldBlock | ErrorKind::TimedOut
                            ) =>
                        {
                            continue;
                        }
                        Err(error) => {
                            tracing::warn!(%error, "HomeKit SRTP relay receive failed");
                            break;
                        }
                    };
                    if source.ip() != producer_ip
                        || source == destination
                        || length < 8
                        || buffer[0] >> 6 != 2
                    {
                        continue;
                    }
                    let mut packet = buffer[..length].to_vec();
                    let is_rtcp = (192..=223).contains(&buffer[1]);
                    let protected = if is_rtcp {
                        srtcp.protect(&mut packet)
                    } else if length >= 12 && buffer[1] & 0x7f == payload_type {
                        srtp.protect(&mut packet)
                    } else {
                        continue;
                    };
                    if let Err(error) = protected {
                        tracing::warn!(%error, is_rtcp, "HomeKit media packet protection failed");
                        continue;
                    }
                    match socket.send_to(&packet, destination) {
                        Ok(sent) => {
                            bytes = bytes.saturating_add(sent as u64);
                            if is_rtcp {
                                rtcp_packets = rtcp_packets.saturating_add(1);
                            } else {
                                rtp_packets = rtp_packets.saturating_add(1);
                            }
                            if !is_rtcp && rtp_packets == 1 {
                                tracing::info!(
                                    %destination,
                                    bytes = sent,
                                    "first HomeKit SRTP video packet sent"
                                );
                            } else if is_rtcp && rtcp_packets == 1 {
                                tracing::info!(
                                    %destination,
                                    bytes = sent,
                                    "first HomeKit SRTCP video report sent"
                                );
                            }
                        }
                        Err(error) => {
                            tracing::warn!(%error, %destination, "HomeKit SRTP relay send failed");
                            break;
                        }
                    }
                }
                tracing::info!(
                    rtp_packets,
                    rtcp_packets,
                    bytes,
                    "HomeKit secure video relay stopped"
                );
            })?;
        Ok(Self { stop, handle })
    }

    fn stop(self) {
        self.stop.store(true, Ordering::Release);
        if self.handle.join().is_err() {
            tracing::warn!("HomeKit SRTP relay thread panicked");
        }
    }
}

impl Drop for LegacyRtpSession {
    fn drop(&mut self) {
        self.stop();
    }
}

fn ensure_session_id(
    session: &LegacyRtpSession,
    session_id: LegacySessionId,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        session.request.session_id == session_id,
        "Selected RTP Stream session identifier does not match Setup Endpoints"
    );
    Ok(())
}

fn bind_return_socket(local_ip: IpAddr) -> std::io::Result<UdpSocket> {
    UdpSocket::bind(SocketAddr::new(local_ip, 0))
}

fn routed_local_ip(destination: SocketAddr, fallback: IpAddr) -> IpAddr {
    let bind_ip = match destination.ip() {
        IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    };
    UdpSocket::bind(SocketAddr::new(bind_ip, 0))
        .and_then(|socket| {
            socket.connect(destination)?;
            socket.local_addr()
        })
        .map_or(fallback, |address| address.ip())
}

fn nonzero_random_u32() -> u32 {
    rand::random::<u32>().max(1)
}

fn random_srtp_parameters() -> LegacySrtpParameters {
    LegacySrtpParameters {
        master_key: rand::random(),
        master_salt: rand::random(),
    }
}

const fn setup_iid_for_selected(selected_iid: u64) -> Option<u64> {
    match selected_iid {
        43 => Some(44),
        51 => Some(52),
        _ => None,
    }
}

fn ffmpeg_args(
    input: &FfmpegInput,
    video: &LegacyVideoParameters,
    prepared_ssrc: u32,
    relay_address: SocketAddr,
    force_hevc: bool,
) -> Vec<String> {
    let mut args = vec![
        "-hide_banner".to_owned(),
        "-nostdin".to_owned(),
        "-loglevel".to_owned(),
        "warning".to_owned(),
    ];
    match input {
        FfmpegInput::File(path) => args.extend([
            "-re".to_owned(),
            "-stream_loop".to_owned(),
            "-1".to_owned(),
            "-i".to_owned(),
            path.to_string_lossy().into_owned(),
        ]),
        FfmpegInput::Rtsp { url, transport } => args.extend([
            "-rtsp_transport".to_owned(),
            (*transport).to_owned(),
            "-i".to_owned(),
            url.clone(),
        ]),
    }
    let frame_rate = video.frame_rate.max(1);
    let maximum_bitrate = video.maximum_bitrate_kbps.max(64);
    let buffer_size = u32::from(maximum_bitrate).saturating_mul(2);
    let keyframe_interval = u16::from(frame_rate).saturating_mul(2);
    let packet_size = video
        .maximum_mtu
        .unwrap_or(DEFAULT_RTP_PACKET_SIZE)
        .clamp(MIN_RTP_PACKET_SIZE, MAX_RTP_PACKET_SIZE);
    let scale = format!(
        "scale={}:{}:force_original_aspect_ratio=decrease,pad={}:{}:(ow-iw)/2:(oh-ih)/2,format=yuv420p",
        video.width, video.height, video.width, video.height
    );
    let profile = match video.profile {
        LegacyH264Profile::ConstrainedBaseline => "baseline",
        LegacyH264Profile::Main => "main",
        LegacyH264Profile::High => "high",
    };
    let level = match video.level {
        LegacyH264Level::Level31 => "3.1",
        LegacyH264Level::Level32 => "3.2",
        LegacyH264Level::Level40 => "4.0",
    };
    let host = match relay_address.ip() {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    };
    let destination = format!(
        "rtp://{host}:{}?rtcpport={}&pkt_size={packet_size}",
        relay_address.port(),
        relay_address.port()
    );
    args.extend([
        "-map".to_owned(),
        "0:v:0".to_owned(),
        "-an".to_owned(),
        "-sn".to_owned(),
        "-dn".to_owned(),
        "-vf".to_owned(),
        scale,
    ]);
    if force_hevc || video.codec == 1 {
        args.extend([
            "-c:v".to_owned(),
            "libx265".to_owned(),
            "-preset".to_owned(),
            "ultrafast".to_owned(),
            "-tune".to_owned(),
            "zerolatency".to_owned(),
            "-x265-params".to_owned(),
            "repeat-headers=1:keyint=60:min-keyint=30".to_owned(),
            "-tag:v".to_owned(),
            "hvc1".to_owned(),
        ]);
    } else {
        args.extend([
            "-c:v".to_owned(),
            "libx264".to_owned(),
            "-profile:v".to_owned(),
            profile.to_owned(),
            "-level:v".to_owned(),
            level.to_owned(),
            "-color_range".to_owned(),
            "mpeg".to_owned(),
            "-preset".to_owned(),
            "superfast".to_owned(),
            "-tune".to_owned(),
            "zerolatency".to_owned(),
            "-x264-params".to_owned(),
            "sliced-threads=0:slices=1".to_owned(),
        ]);
    }
    args.extend([
        "-r".to_owned(),
        frame_rate.to_string(),
        "-g".to_owned(),
        keyframe_interval.to_string(),
        "-keyint_min".to_owned(),
        frame_rate.to_string(),
        "-sc_threshold".to_owned(),
        "0".to_owned(),
        "-b:v".to_owned(),
        format!("{maximum_bitrate}k"),
        "-maxrate".to_owned(),
        format!("{maximum_bitrate}k"),
        "-bufsize".to_owned(),
        format!("{buffer_size}k"),
        // `-f rtp` sets a global hecontroller has no parameter
        // sets and shows a spinner forever.
        "-bsf:v".to_owned(),
        "dump_extra=freq=keyframe".to_owned(),
        "-payload_type".to_owned(),
        video.payload_type.to_string(),
        "-ssrc".to_owned(),
        prepared_ssrc.to_string(),
        "-f".to_owned(),
        "rtp".to_owned(),
        destination,
    ]);
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_request() -> SetupEndpointsRequest {
        SetupEndpointsRequest {
            session_id: LegacySessionId::new([7; 16]),
            controller: LegacyRtpAddress {
                ip: "192.0.2.20".parse().unwrap(),
                video_port: 50_000,
                audio_port: 50_001,
            },
            video_srtp: LegacySrtpParameters {
                master_key: [3; 16],
                master_salt: [4; 14],
            },
            audio_srtp: LegacySrtpParameters {
                master_key: [5; 16],
                master_salt: [6; 14],
            },
        }
    }

    #[test]
    fn ffmpeg_sends_plain_rtp_to_the_local_relay() {
        let input = FfmpegInput::File("fixture.mp4".into());
        let video = LegacyVideoParameters {
            codec: 0,
            profile: LegacyH264Profile::Main,
            level: LegacyH264Level::Level31,
            payload_type: 99,
            ssrc: 0x1122_3344,
            maximum_bitrate_kbps: 800,
            rtcp_interval_seconds: 0.5,
            maximum_mtu: Some(1_200),
            width: 1280,
            height: 720,
            frame_rate: 30,
        };

        let prepared_ssrc = 0x5566_7788;
        let args = ffmpeg_args(
            &input,
            &video,
            prepared_ssrc,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 40_000),
            false,
        );

        assert!(args.windows(2).any(|pair| pair == ["-payload_type", "99"]));
        assert!(args.windows(2).any(|pair| pair == ["-ssrc", "1432778632"]));
        assert!(args.windows(2).any(|pair| pair == ["-preset", "superfast"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-x264-params", "sliced-threads=0:slices=1"])
        );
        assert!(!args.iter().any(|argument| argument == "skip_rtcp"));
        assert!(!args.iter().any(|argument| argument.contains("srtp")));
        assert_eq!(
            args.last().unwrap(),
            "rtp://127.0.0.1:40000?rtcpport=40000&pkt_size=1200"
        );
    }

    #[test]
    fn ffmpeg_encodes_hevc_when_forced() {
        let input = FfmpegInput::File("fixture.mp4".into());
        let video = LegacyVideoParameters {
            codec: 0,
            profile: LegacyH264Profile::Main,
            level: LegacyH264Level::Level31,
            payload_type: 99,
            ssrc: 0x1122_3344,
            maximum_bitrate_kbps: 800,
            rtcp_interval_seconds: 0.5,
            maximum_mtu: Some(1_200),
            width: 1280,
            height: 720,
            frame_rate: 30,
        };

        let args = ffmpeg_args(
            &input,
            &video,
            0x5566_7788,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 40_000),
            true,
        );

        assert!(args.windows(2).any(|pair| pair == ["-c:v", "libx265"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-x265-params", "repeat-headers=1:keyint=60:min-keyint=30"])
        );
        assert!(!args.iter().any(|argument| argument == "libx264"));
    }

    #[test]
    fn relay_protects_rtp_and_sends_from_the_advertised_port() {
        let controller = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        controller
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let accessory = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let accessory_address = SocketAddr::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            accessory.local_addr().unwrap().port(),
        );
        let parameters = setup_request().video_srtp;
        let relay = SrtpRelay::start(
            accessory,
            controller.local_addr().unwrap(),
            parameters.clone(),
            99,
        )
        .unwrap();
        let ffmpeg = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut plaintext = vec![0x80, 99, 0, 7];
        plaintext.extend_from_slice(&900_u32.to_be_bytes());
        plaintext.extend_from_slice(&0x5566_7788_u32.to_be_bytes());
        plaintext.extend_from_slice(&[0x65, 1, 2, 3]);

        ffmpeg.send_to(&plaintext, accessory_address).unwrap();

        let mut received = [0_u8; 2_048];
        let (length, source) = controller.recv_from(&mut received).unwrap();
        let mut expected = plaintext;
        SrtpSession::new(&parameters.master_key, &parameters.master_salt)
            .protect(&mut expected)
            .unwrap();
        assert_eq!(&received[..length], expected);
        assert_eq!(source.port(), accessory_address.port());

        let mut report = vec![0x80, 200, 0, 6];
        report.extend_from_slice(&0x5566_7788_u32.to_be_bytes());
        report.extend_from_slice(&[0x11; 20]);
        ffmpeg.send_to(&report, accessory_address).unwrap();
        let (length, source) = controller.recv_from(&mut received).unwrap();
        let mut expected = report;
        SrtcpSession::new(&parameters.master_key, &parameters.master_salt)
            .protect(&mut expected)
            .unwrap();
        assert_eq!(&received[..length], expected);
        assert_eq!(source.port(), accessory_address.port());
        relay.stop();
    }

    #[test]
    fn media_route_uses_the_controller_facing_address() {
        let controller = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();

        let selected = routed_local_ip(
            controller.local_addr().unwrap(),
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        );

        assert_eq!(selected, IpAddr::V4(Ipv4Addr::LOCALHOST));
    }
}
