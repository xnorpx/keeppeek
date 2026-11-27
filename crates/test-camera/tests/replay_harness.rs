//! Integration tests for `ReplayCameraServer` and `CameraReproFixture`.

use onvif::soap::client::{AuthType, ClientBuilder, Credentials};
use reo_proto::{
    BcSession, Command, EncryptionMode, Event, Input, LoginParams, Output, StreamSubscription,
    StreamType, VideoCodec,
};
use retina::{
    client::{
        Credentials as RtspCredentials,
        core::{
            ClientOptions, ClientState, Command as RtspCommand, Event as RtspEvent,
            Input as RtspInput, Output as RtspOutput, RtspClient, Time, UdpPacketKind,
        },
    },
    codec::CodecItem,
};
use rouille::url::Url;
use schema::{devicemgmt, media as onvif_media};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, UdpSocket},
    time::{Duration, Instant},
};
use test_camera::{
    CameraReproFixture, HttpExchangeFixture, HttpReproFixture, REPRO_SCHEMA_VERSION,
    RecordedProtocol, ReoFrameFixture, ReoProtoReproFixture, ReplayCameraServer,
    ReplayServerOptions, ReproCameraInfo, ReproProtocol, RtpPacketFixture, RtspReproFixture,
    RtspResponseFixture, RtspSessionFixture, encode_base64,
};

const H264_SPS_B64: &str = "Z0IAF+WIaEA=";
const H264_PPS_B64: &str = "aM48gA==";

fn h264_sdp() -> String {
    format!(
        "v=0\r\n\
o=- 1 1 IN IP4 192.0.2.10\r\n\
s=Replay Cam\r\n\
t=0 0\r\n\
a=control:*\r\n\
m=video 0 RTP/AVP 96\r\n\
a=rtpmap:96 H264/90000\r\n\
a=fmtp:96 profile-level-id=42001f; packetization-mode=1; sprop-parameter-sets={H264_SPS_B64},{H264_PPS_B64}\r\n\
a=control:trackID=0\r\n"
    )
}

fn rtsp_h264_fixture(packets: Vec<RtpPacketFixture>) -> CameraReproFixture {
    CameraReproFixture {
        schema_version: REPRO_SCHEMA_VERSION.to_owned(),
        camera: ReproCameraInfo {
            name: Some("rtsp-repro".to_owned()),
            protocol: ReproProtocol::Rtsp,
            anonymized: true,
            credentials_stripped: true,
            base_host: "192.0.2.10".to_owned(),
        },
        rtsp: Some(RtspReproFixture {
            sessions: vec![RtspSessionFixture {
                setup_order: Some(vec!["video".to_owned()]),
                responses: vec![
                    RtspResponseFixture {
                        method: Some("DESCRIBE".to_owned()),
                        uri_contains: None,
                        status: 200,
                        reason: "OK".to_owned(),
                        headers: vec![("Content-Type".to_owned(), "application/sdp".to_owned())],
                        body: h264_sdp(),
                    },
                    RtspResponseFixture {
                        method: Some("SETUP".to_owned()),
                        uri_contains: Some("trackID=0".to_owned()),
                        status: 200,
                        reason: "OK".to_owned(),
                        headers: vec![("Session".to_owned(), "repro-42;timeout=60".to_owned())],
                        body: String::new(),
                    },
                    RtspResponseFixture {
                        method: Some("PLAY".to_owned()),
                        uri_contains: None,
                        status: 200,
                        reason: "OK".to_owned(),
                        headers: vec![(
                            "RTP-Info".to_owned(),
                            "url=trackID=0;seq=1;rtptime=90000".to_owned(),
                        )],
                        body: String::new(),
                    },
                ],
                rtp_packets: packets,
            }],
        }),
        http: None,
        reo_proto: None,
    }
}

fn onvif_and_reolink_fixture() -> CameraReproFixture {
    CameraReproFixture {
        schema_version: REPRO_SCHEMA_VERSION.to_owned(),
        camera: ReproCameraInfo {
            name: Some("quirky-onvif".to_owned()),
            protocol: ReproProtocol::Onvif,
            anonymized: true,
            credentials_stripped: true,
            base_host: "192.0.2.20".to_owned(),
        },
        rtsp: None,
        http: Some(HttpReproFixture {
            exchanges: vec![
                HttpExchangeFixture {
                    match_method: Some("POST".to_owned()),
                    match_path_contains: Some("/onvif/device_service".to_owned()),
                    match_soap_action: None,
                    match_body_contains: Some("GetDeviceInformation".to_owned()),
                    status: 200,
                    status_reason: Some("Quirky Camera OK".to_owned()),
                    headers: vec![
                        ("Content-Type".to_owned(), "application/soap+xml".to_owned()),
                        ("X-Camera-Quirk".to_owned(), "legacy-soap".to_owned()),
                    ],
                    body_text: Some(
                        r#"<?xml version="1.0" encoding="UTF-8"?><s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:tds="http://www.onvif.org/ver10/device/wsdl"><s:Body><tds:GetDeviceInformationResponse><tds:Manufacturer>QuirkCam</tds:Manufacturer><tds:Model>QC-9000</tds:Model><tds:FirmwareVersion>1.0-repro</tds:FirmwareVersion><tds:SerialNumber>SN-REPRO</tds:SerialNumber><tds:HardwareId>HW-1</tds:HardwareId></tds:GetDeviceInformationResponse></s:Body></s:Envelope>"#.to_owned(),
                    ),
                    body_base64: None,
                    delay_ms: None,
                },
                HttpExchangeFixture {
                    match_method: Some("POST".to_owned()),
                    match_path_contains: Some("/onvif/media_service".to_owned()),
                    match_soap_action: None,
                    match_body_contains: Some("GetProfiles".to_owned()),
                    status: 200,
                    status_reason: None,
                    headers: vec![("Content-Type".to_owned(), "application/soap+xml".to_owned())],
                    body_text: Some(
                        r#"<?xml version="1.0" encoding="UTF-8"?><s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope" xmlns:trt="http://www.onvif.org/ver10/media/wsdl" xmlns:tt="http://www.onvif.org/ver10/schema"><s:Body><trt:GetProfilesResponse><trt:Profiles token="main"><tt:Name>Main</tt:Name><tt:VideoEncoderConfiguration token="v0"><tt:Name>MainV</tt:Name><tt:UseCount>1</tt:UseCount><tt:Encoding>H264</tt:Encoding><tt:Resolution><tt:Width>1280</tt:Width><tt:Height>720</tt:Height></tt:Resolution><tt:Quality>5</tt:Quality><tt:Multicast><tt:Address><tt:Type>IPv4</tt:Type><tt:IPv4Address>0.0.0.0</tt:IPv4Address></tt:Address><tt:Port>0</tt:Port><tt:TTL>0</tt:TTL><tt:AutoStart>false</tt:AutoStart></tt:Multicast><tt:SessionTimeout>PT0S</tt:SessionTimeout></tt:VideoEncoderConfiguration></trt:Profiles></trt:GetProfilesResponse></s:Body></s:Envelope>"#.to_owned(),
                    ),
                    body_base64: None,
                    delay_ms: None,
                },
                HttpExchangeFixture {
                    match_method: Some("POST".to_owned()),
                    match_path_contains: Some("cmd=GetDevInfo".to_owned()),
                    match_soap_action: None,
                    match_body_contains: Some("GetDevInfo".to_owned()),
                    status: 200,
                    status_reason: None,
                    headers: vec![("Content-Type".to_owned(), "application/json".to_owned())],
                    body_text: Some(
                        r#"[{"cmd":"GetDevInfo","code":0,"value":{"DevInfo":{"model":"RLC-Repro"}}}]"#.to_owned(),
                    ),
                    body_base64: None,
                    delay_ms: None,
                },
            ],
        }),
        reo_proto: None,
    }
}

#[test]
fn replays_quirky_onvif_and_reolink_http_with_real_onvif_client() {
    let server = ReplayCameraServer::start(onvif_and_reolink_fixture()).unwrap();
    let base_url = server.http_url().unwrap();
    let creds = Some(Credentials {
        username: "u".to_owned(),
        password: "p".to_owned(),
    });
    let dev_url = Url::parse(&format!("{base_url}/onvif/device_service")).unwrap();
    let dev_client = ClientBuilder::new(&dev_url)
        .credentials(creds.clone())
        .auth_type(AuthType::UsernameToken)
        .build();
    let info =
        devicemgmt::get_device_information(&dev_client, &devicemgmt::GetDeviceInformation {})
            .unwrap();
    assert_eq!(info.manufacturer, "QuirkCam");
    assert_eq!(info.model, "QC-9000");

    let media_url = Url::parse(&format!("{base_url}/onvif/media_service")).unwrap();
    let media_client = ClientBuilder::new(&media_url)
        .credentials(creds)
        .auth_type(AuthType::UsernameToken)
        .build();
    let profiles = onvif_media::get_profiles(&media_client, &onvif_media::GetProfiles {}).unwrap();
    assert_eq!(profiles.profiles.len(), 1);
    assert_eq!(profiles.profiles[0].token.0, "main");

    let mut tcp = TcpStream::connect(server.http_addr().unwrap()).unwrap();
    let body = r#"[{"cmd":"GetDevInfo","action":0,"param":{}}]"#;
    let req = format!(
        "POST /cgi-bin/api.cgi?cmd=GetDevInfo HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    tcp.write_all(req.as_bytes()).unwrap();
    let mut raw = String::new();
    tcp.read_to_string(&mut raw).unwrap();
    assert!(raw.contains("RLC-Repro"));

    let recorded = server.recorded_requests();
    assert_eq!(recorded.len(), 3);
    assert!(
        recorded
            .iter()
            .all(|r| r.protocol == RecordedProtocol::Http)
    );
    assert!(server.is_completed());
}

#[test]
fn replays_rtsp_h264_over_tcp_and_udp_to_real_retina_client() {
    let idr_nal = [0x65_u8, 0x88, 0x84, 0x21, 0x40];
    let p_nal = [0x41_u8, 0x9a, 0x22, 0x11, 0x00];
    let fixture = rtsp_h264_fixture(vec![
        RtpPacketFixture {
            channel: Some(0),
            track: "video".to_owned(),
            marker: true,
            payload_type: 96,
            sequence: 1,
            timestamp: 90_000,
            ssrc: 0x1122_3344,
            delay_ms: None,
            payload_base64: encode_base64(&idr_nal),
        },
        RtpPacketFixture {
            channel: Some(0),
            track: "video".to_owned(),
            marker: true,
            payload_type: 96,
            sequence: 2,
            timestamp: 93_000,
            ssrc: 0x1122_3344,
            delay_ms: Some(5),
            payload_base64: encode_base64(&p_nal),
        },
    ]);

    for use_udp in [false, true] {
        let server = ReplayCameraServer::start(fixture.clone()).unwrap();
        let url = Url::parse(&format!("{}/stream", server.rtsp_url().unwrap())).unwrap();
        let frames = drive_retina_client(url, use_udp);
        assert_eq!(frames.len(), 2, "expected 2 H.264 frames (udp={use_udp})");
        assert!(server.is_completed());
        let recorded = server.recorded_requests();
        assert!(recorded.iter().any(|r| r.method == "DESCRIBE"));
        assert!(recorded.iter().any(|r| r.method == "SETUP"));
        assert!(recorded.iter().any(|r| r.method == "PLAY"));
    }
}

fn malformed_rtp_fixture(malformed_fua_start: &[u8], truncated_fua: &[u8]) -> CameraReproFixture {
    rtsp_h264_fixture(vec![
        RtpPacketFixture {
            channel: Some(0),
            track: "video".to_owned(),
            marker: false,
            payload_type: 96,
            sequence: 10,
            timestamp: 90_000,
            ssrc: 0x0a0b_0c0d,
            delay_ms: None,
            payload_base64: encode_base64(malformed_fua_start),
        },
        RtpPacketFixture {
            channel: Some(0),
            track: "video".to_owned(),
            marker: true,
            payload_type: 96,
            sequence: 15,
            timestamp: 45_000,
            ssrc: 0x0a0b_0c0d,
            delay_ms: None,
            payload_base64: encode_base64(truncated_fua),
        },
        RtpPacketFixture {
            channel: Some(0),
            track: "video".to_owned(),
            marker: true,
            payload_type: 96,
            sequence: 14,
            timestamp: 45_000,
            ssrc: 0x0a0b_0c0d,
            delay_ms: None,
            payload_base64: encode_base64(&[0x65, 0x88]),
        },
    ])
}

#[test]
fn replays_exact_malformed_rtp_sequence_and_fua_fragments() {
    let malformed_fua_start = [0x7c_u8, 0x85, 0xde, 0xad];
    let truncated_fua = [0x7c_u8];
    let server =
        ReplayCameraServer::start(malformed_rtp_fixture(&malformed_fua_start, &truncated_fua))
            .unwrap();
    let addr = server.rtsp_addr().unwrap();
    let mut tcp = TcpStream::connect(addr).unwrap();
    tcp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let url = format!("rtsp://{addr}/stream");
    let mut buf = Vec::new();
    for raw in [
        format!("DESCRIBE {url} RTSP/1.0\r\nCSeq: 1\r\nAccept: application/sdp\r\n\r\n"),
        format!(
            "SETUP {url}/trackID=0 RTSP/1.0\r\nCSeq: 2\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n\r\n"
        ),
        format!("PLAY {url} RTSP/1.0\r\nCSeq: 3\r\nSession: repro-42\r\n\r\n"),
    ] {
        write_rtsp(&mut tcp, &raw);
        assert!(read_rtsp_head(&mut tcp, &mut buf).contains("200 OK"));
    }
    let observed: Vec<_> = (0..3)
        .map(|_| {
            let rtp = read_interleaved_rtp(&mut tcp, &mut buf);
            let seq = u16::from_be_bytes([rtp[2], rtp[3]]);
            let ts = u32::from_be_bytes([rtp[4], rtp[5], rtp[6], rtp[7]]);
            (seq, ts, rtp[12..].to_vec())
        })
        .collect();
    assert_eq!(
        observed,
        vec![
            (10, 90_000, malformed_fua_start.to_vec()),
            (15, 45_000, truncated_fua.to_vec()),
            (14, 45_000, vec![0x65, 0x88]),
        ]
    );
}

fn reo_h264_iframe_payload(nal: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&reo_proto::media::MEDIA_MAGIC_INFO_V1.to_le_bytes());
    payload.extend_from_slice(&30_u32.to_le_bytes());
    payload.extend_from_slice(&640_u32.to_le_bytes());
    payload.extend_from_slice(&360_u32.to_le_bytes());
    payload.push(0);
    payload.push(15);
    payload.extend_from_slice(&[26, 1, 1, 0, 0, 0, 26, 1, 1, 0, 0, 0]);
    while !payload.len().is_multiple_of(8) {
        payload.push(0);
    }
    payload.extend_from_slice(&reo_proto::media::MEDIA_MAGIC_IFRAME_BASE.to_le_bytes());
    payload.extend_from_slice(b"H264");
    payload.extend_from_slice(&(nal.len() as u32).to_le_bytes());
    payload.extend_from_slice(&0_u32.to_le_bytes());
    payload.extend_from_slice(&1000_u32.to_le_bytes());
    payload.extend_from_slice(&0_u32.to_le_bytes());
    payload.extend_from_slice(nal);
    while !payload.len().is_multiple_of(8) {
        payload.push(0);
    }
    payload
}

fn reo_replay_fixture(media_payload: &[u8]) -> CameraReproFixture {
    CameraReproFixture {
        schema_version: REPRO_SCHEMA_VERSION.to_owned(),
        camera: ReproCameraInfo {
            name: Some("reo-repro".to_owned()),
            protocol: ReproProtocol::ReoProto,
            anonymized: true,
            credentials_stripped: true,
            base_host: "192.0.2.30".to_owned(),
        },
        rtsp: None,
        http: None,
        reo_proto: Some(ReoProtoReproFixture {
            frames: vec![
                ReoFrameFixture {
                    expected_msg_id: Some(1),
                    response_msg_id: 1,
                    payload_base64: None,
                    payload_xml: Some(
                        r#"<body><Encryption version="1"><type>none</type><nonce>REPRO_NONCE</nonce></Encryption></body>"#.to_owned(),
                    ),
                    channel_id: Some(0),
                    stream_type: Some(0),
                    delay_ms: None,
                },
                ReoFrameFixture {
                    expected_msg_id: Some(1),
                    response_msg_id: 1,
                    payload_base64: None,
                    payload_xml: Some(
                        r#"<body><LoginUser version="2"><userName>test</userName><result>ok</result><userId>1</userId></LoginUser><DeviceInfo version="2"><model>RLC-Repro</model><serialNumber>SN-REPRO</serialNumber><firmVer>1.0</firmVer><channelNum>1</channelNum></DeviceInfo></body>"#.to_owned(),
                    ),
                    channel_id: Some(0),
                    stream_type: Some(0),
                    delay_ms: None,
                },
                ReoFrameFixture {
                    expected_msg_id: Some(3),
                    response_msg_id: 3,
                    payload_base64: None,
                    payload_xml: Some(
                        r#"<body><Preview version="1.1"><channelId>0</channelId><handle>0</handle><streamType>mainStream</streamType></Preview></body>"#.to_owned(),
                    ),
                    channel_id: Some(0),
                    stream_type: Some(0),
                    delay_ms: None,
                },
                ReoFrameFixture {
                    expected_msg_id: None,
                    response_msg_id: 3,
                    payload_base64: Some(encode_base64(media_payload)),
                    payload_xml: None,
                    channel_id: Some(0),
                    stream_type: Some(0),
                    delay_ms: None,
                },
            ],
        }),
    }
}

#[test]
fn replays_reo_proto_login_and_video_frame_to_real_bc_session() {
    let nal = [0_u8, 0, 0, 1, 0x65, 0x88, 0x84, 0x21];
    let media_payload = reo_h264_iframe_payload(&nal);
    let server = ReplayCameraServer::start(reo_replay_fixture(&media_payload)).unwrap();
    let mut stream = TcpStream::connect(server.reo_addr().unwrap()).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();

    let mut client = BcSession::default_client(Instant::now());
    client
        .handle_input(Input::Command(Command::Login(LoginParams::new(
            "test",
            "test",
            EncryptionMode::None,
        ))))
        .unwrap();

    let mut out = vec![0_u8; reo_proto::DEFAULT_MEDIA_OUTPUT_BUFFER_SIZE];
    let mut buf = [0_u8; 64 * 1024];
    let mut saw_frame = false;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && !saw_frame {
        saw_frame |= drain_bc_outputs(&mut client, &mut stream, &mut out, &nal);
        if !saw_frame && !pump_bc_tcp(&mut client, &mut stream, &mut buf) {
            break;
        }
    }
    assert!(saw_frame, "BcSession did not receive replayed video frame");
    assert!(server.is_completed());
    assert!(
        server
            .recorded_requests()
            .iter()
            .any(|r| r.protocol == RecordedProtocol::ReoProto && r.method == "msg_id:3")
    );
}

fn drain_bc_outputs(
    client: &mut BcSession,
    stream: &mut TcpStream,
    out: &mut [u8],
    expected_nal: &[u8],
) -> bool {
    let mut saw_frame = false;
    loop {
        match client.poll_output(out).unwrap() {
            Output::TcpSend { data } => stream.write_all(data).unwrap(),
            Output::Event(Event::LoggedIn(_)) => {
                client
                    .handle_input(Input::Command(Command::SubscribeStream(
                        StreamSubscription {
                            channel: 0,
                            stream_type: StreamType::Main,
                            expected_width: 640,
                            expected_height: 360,
                        },
                    )))
                    .unwrap();
            }
            Output::Event(Event::VideoFrame { codec, data, .. }) => {
                assert_eq!(codec, VideoCodec::H264);
                assert_eq!(data, expected_nal);
                saw_frame = true;
            }
            Output::Event(_) | Output::Timeout(_) => return saw_frame,
        }
    }
}

fn pump_bc_tcp(client: &mut BcSession, stream: &mut TcpStream, buf: &mut [u8]) -> bool {
    match stream.read(buf) {
        Ok(n) if n > 0 => {
            client
                .handle_input(Input::TcpData(Instant::now(), &buf[..n]))
                .unwrap();
            true
        }
        Ok(_) => false,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) =>
        {
            client.handle_input(Input::Timeout(Instant::now())).unwrap();
            true
        }
        Err(e) => panic!("reo replay read failed: {e}"),
    }
}

#[test]
fn drop_releases_ports_cleanly_for_reuse() {
    let fixture = CameraReproFixture {
        schema_version: REPRO_SCHEMA_VERSION.to_owned(),
        camera: ReproCameraInfo {
            name: None,
            protocol: ReproProtocol::Isapi,
            anonymized: true,
            credentials_stripped: true,
            base_host: "192.0.2.40".to_owned(),
        },
        rtsp: None,
        http: Some(HttpReproFixture { exchanges: vec![] }),
        reo_proto: None,
    };
    let server = ReplayCameraServer::start(fixture.clone()).unwrap();
    let addr = server.http_addr().unwrap();
    drop(server);

    let rebound = ReplayCameraServer::start_with_options(
        fixture,
        ReplayServerOptions {
            bind_addr: addr,
            loop_replay: false,
        },
    )
    .unwrap();
    assert_eq!(rebound.http_addr(), Some(addr));
}

fn now_time() -> Time {
    Time {
        monotonic: Instant::now(),
        wall: std::time::SystemTime::now(),
    }
}

fn drive_retina_client(url: Url, use_udp: bool) -> Vec<Vec<u8>> {
    let mut client = RtspClient::new(ClientOptions {
        credentials: Some(RtspCredentials {
            username: "u".to_owned(),
            password: "p".to_owned(),
        }),
        ..ClientOptions::default()
    });
    let mut tcp: Option<(retina::client::core::TcpConnectionId, TcpStream)> = None;
    let mut udp_pair: Option<(UdpSocket, UdpSocket)> = None;
    let mut setup_sent = false;
    let mut play_sent = false;
    let mut frames = VecDeque::new();
    let mut buf = [0_u8; 64 * 1024];

    client
        .handle_input(RtspInput::Command {
            time: now_time(),
            command: RtspCommand::Describe { url },
        })
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && frames.len() < 2 {
        drain_retina_outputs(&mut client, &mut tcp, &mut frames);
        advance_retina_step(
            &mut client,
            &mut udp_pair,
            &mut setup_sent,
            &mut play_sent,
            use_udp,
        );
        poll_retina_sockets(&mut client, tcp.as_mut(), udp_pair.as_ref(), &mut buf);
    }
    frames.into_iter().collect()
}

fn drain_retina_outputs(
    client: &mut RtspClient,
    tcp: &mut Option<(retina::client::core::TcpConnectionId, TcpStream)>,
    frames: &mut VecDeque<Vec<u8>>,
) {
    loop {
        match client.poll_output() {
            RtspOutput::OpenTcp { connection, target } => {
                let s = TcpStream::connect((target.host.as_ref(), target.port)).unwrap();
                s.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
                *tcp = Some((connection, s));
                client
                    .handle_input(RtspInput::TcpConnected {
                        time: now_time(),
                        connection,
                    })
                    .unwrap();
            }
            RtspOutput::TcpTransmit { connection, data } => {
                if let Some((_, s)) = tcp.as_mut() {
                    s.write_all(&data).unwrap();
                }
                client
                    .handle_input(RtspInput::TcpWriteCompleted {
                        time: now_time(),
                        connection,
                    })
                    .unwrap();
            }
            RtspOutput::Event(RtspEvent::CodecItem(CodecItem::VideoFrame(f))) => {
                frames.push_back(f.into_data());
            }
            RtspOutput::Timeout(_) => break,
            _ => {}
        }
    }
}

fn advance_retina_step(
    client: &mut RtspClient,
    udp_pair: &mut Option<(UdpSocket, UdpSocket)>,
    setup_sent: &mut bool,
    play_sent: &mut bool,
    use_udp: bool,
) {
    if client.state() == ClientState::Described && !*setup_sent {
        *setup_sent = true;
        let command = if use_udp {
            let (rtp, rtcp) = bind_client_udp();
            let client_port = rtp.local_addr().unwrap().port();
            *udp_pair = Some((rtp, rtcp));
            RtspCommand::SetupUdp {
                stream: 0,
                client_port,
            }
        } else {
            RtspCommand::Setup { stream: 0 }
        };
        client
            .handle_input(RtspInput::Command {
                time: now_time(),
                command,
            })
            .unwrap();
    } else if client.state() == ClientState::Described && *setup_sent && !*play_sent {
        *play_sent = true;
        client
            .handle_input(RtspInput::Command {
                time: now_time(),
                command: RtspCommand::Play,
            })
            .unwrap();
    }
}

fn poll_retina_sockets(
    client: &mut RtspClient,
    tcp: Option<&mut (retina::client::core::TcpConnectionId, TcpStream)>,
    udp_pair: Option<&(UdpSocket, UdpSocket)>,
    buf: &mut [u8],
) {
    if let Some((conn_id, s)) = tcp
        && let Ok(n) = s.read(buf)
        && n > 0
    {
        client
            .handle_input(RtspInput::TcpData {
                time: now_time(),
                connection: *conn_id,
                data: &buf[..n],
            })
            .unwrap();
    }
    if let Some((rtp, _)) = udp_pair
        && let Ok(n) = rtp.recv(buf)
    {
        client
            .handle_input(RtspInput::UdpData {
                time: now_time(),
                stream: 0,
                kind: UdpPacketKind::Rtp,
                data: &buf[..n],
            })
            .unwrap();
    }
}

fn bind_client_udp() -> (UdpSocket, UdpSocket) {
    for _ in 0..100 {
        let rtp = UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap();
        let port = rtp.local_addr().unwrap().port();
        if let Some(rtcp_port) = port.checked_add(1)
            && let Ok(rtcp) =
                UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), rtcp_port))
        {
            rtp.set_read_timeout(Some(Duration::from_millis(20)))
                .unwrap();
            return (rtp, rtcp);
        }
    }
    panic!("unable to bind client UDP pair");
}

fn write_rtsp(stream: &mut TcpStream, raw: &str) {
    stream.write_all(raw.as_bytes()).unwrap();
    stream.flush().unwrap();
}

fn read_rtsp_head(stream: &mut TcpStream, buf: &mut Vec<u8>) -> String {
    loop {
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let end = end + 4;
            let head = String::from_utf8_lossy(&buf[..end]).into_owned();
            let content_len = head
                .lines()
                .find_map(|l| l.strip_prefix("Content-Length: "))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if buf.len() >= end + content_len {
                let full = String::from_utf8_lossy(&buf[..end + content_len]).into_owned();
                buf.drain(..end + content_len);
                return full;
            }
        }
        let mut tmp = [0_u8; 4096];
        let n = stream.read(&mut tmp).unwrap();
        assert!(n > 0, "RTSP connection closed early");
        buf.extend_from_slice(&tmp[..n]);
    }
}

fn read_interleaved_rtp(stream: &mut TcpStream, buf: &mut Vec<u8>) -> Vec<u8> {
    while buf.len() < 4 {
        let mut tmp = [0_u8; 4096];
        let n = stream.read(&mut tmp).unwrap();
        assert!(n > 0, "RTSP stream ended before RTP packet");
        buf.extend_from_slice(&tmp[..n]);
    }
    assert_eq!(buf[0], b'$');
    let len = usize::from(u16::from_be_bytes([buf[2], buf[3]]));
    while buf.len() < 4 + len {
        let mut tmp = [0_u8; 4096];
        let n = stream.read(&mut tmp).unwrap();
        assert!(n > 0, "RTSP stream ended mid-RTP packet");
        buf.extend_from_slice(&tmp[..n]);
    }
    let rtp = buf[4..4 + len].to_vec();
    buf.drain(..4 + len);
    rtp
}
