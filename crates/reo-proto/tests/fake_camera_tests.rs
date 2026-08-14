//! Integration tests using a FakeCamera to exercise the full sans-IO
//! reo-proto protocol stack without any real network I/O.
//!
//! `FakeCamera` wraps a camera-side `BcSession` and implements the
//! server half of the Baichuan protocol: it processes raw bytes from
//! the client session, crafts responses (nonce, login confirmation,
//! stream acks, media frames, snapshots, pings, device queries), and
//! returns wire bytes that can be fed back into the client session.
//!
//! This lets us test complete client↔camera conversations in-process,
//! exercising framing, header serialization, XML generation/parsing,
//! the login handshake, media delivery, and timer-driven behaviour.

use arrayvec::ArrayString;
use reo_proto::{magic::*, media::*, *};
use std::time::{Duration, Instant};

// ── FakeCamera ──────────────────────────────────────────────────────

/// A fake Reolink camera that speaks the Baichuan wire protocol.
///
/// It holds camera-side config (credentials, device info, nonce) and
/// can process raw wire bytes from a client, producing response bytes.
/// All state lives on the stack / in `Vec`s -- no threads, no I/O.
struct FakeCamera {
    expected_user: &'static str,
    expected_pass: &'static str,
    nonce: &'static str,
    encryption: EncryptionMode,
    camera_identity: CameraIdentity,
    user_id: u32,
    /// Whether the camera has authenticated the client.
    authenticated: bool,
    /// Pending response bytes to send back to the client.
    outbox: Vec<u8>,
}

impl FakeCamera {
    fn new() -> Self {
        Self {
            expected_user: "admin",
            expected_pass: "password123",
            nonce: "FAKE_CAMERA_NONCE_42",
            encryption: EncryptionMode::None,
            camera_identity: CameraIdentity {
                model: ArrayString::try_from("RLC-810A").unwrap(),
                serial: ArrayString::try_from("FAKE00112233").unwrap(),
                firmware: ArrayString::try_from("v3.1.0_fake").unwrap(),
                channel_count: 2,
            },
            user_id: 99,
            authenticated: false,
            outbox: Vec::new(),
        }
    }

    /// Feed raw wire bytes from the client into the camera.
    ///
    /// Parses complete Baichuan messages and generates appropriate
    /// responses into `self.outbox`.
    fn receive(&mut self, data: &[u8]) {
        let mut pos = 0;
        while pos < data.len() {
            if pos + HEADER_LEN_SHORT > data.len() {
                break;
            }
            let remaining = &data[pos..];

            let (header, hdr_len) = match PacketHeader::parse(remaining) {
                Ok(v) => v,
                Err(_) => break,
            };

            let total = hdr_len + header.body_len as usize;
            if remaining.len() < total {
                break;
            }

            let body = &remaining[hdr_len..total];
            pos += total;

            self.handle_message(header, body);
        }
    }

    /// Take any pending response bytes out of the camera's outbox.
    fn take_response(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.outbox)
    }

    // ── Message dispatch ─────────────────────────────────────────────

    fn handle_message(&mut self, header: PacketHeader, body: &[u8]) {
        match header.msg_id {
            COMMAND_LOGIN => self.handle_login(header, body),
            COMMAND_PING => self.handle_ping(header),
            COMMAND_STREAM if header.is_modern() => self.handle_stream_start(body),
            COMMAND_PREVIEW_STOP if header.is_modern() => self.handle_stream_stop(body),
            COMMAND_SNAP if header.is_modern() => self.handle_snapshot(body),
            COMMAND_TALK_CAPABILITIES if header.is_modern() => self.handle_talk_ability(body),
            COMMAND_FIRMWARE_DETAILS if header.is_modern() => self.handle_firmware_details(),
            _ => {
                // Unknown or unhandled: send back an empty ack with same msg_id
                self.send_modern_xml(header.msg_id, b"<body></body>");
            }
        }
    }

    // ── Login handshake (camera side) ────────────────────────────────

    fn handle_login(&mut self, header: PacketHeader, body: &[u8]) {
        if header.is_binary() && header.body_len == 0 {
            // Step 1: LoginUpgrade (header-only) → send BCEncrypt'd nonce response
            let mut xml_buf = [0u8; MAX_XML_BODY];
            let xml_len =
                auth::build_nonce_response(self.nonce, self.encryption, &mut xml_buf).unwrap();
            // BCEncrypt the nonce body
            reo_proto::encryption::bc_xor(&mut xml_buf[..xml_len], 0);
            // Camera response_code = 0xDDxx where xx matches the encryption capability
            let camera_rc = (self.encryption.to_class_value() as u16 & 0xFF) | 0xDD00;
            self.send_message(
                COMMAND_LOGIN,
                &xml_buf[..xml_len],
                make_status(0, camera_rc),
            );
        } else if header.is_modern() {
            // Step 3: Modern login → BCEncrypt-decrypt body, validate hashes, send confirmation
            let mut decrypted = body.to_vec();
            reo_proto::encryption::bc_xor(&mut decrypted, 0);
            match auth::parse_modern_login(&decrypted) {
                Ok((user_hash, pass_hash)) => {
                    let valid = auth::validate_credentials(
                        self.nonce,
                        self.expected_user,
                        self.expected_pass,
                        user_hash.as_str(),
                        pass_hash.as_str(),
                    );

                    if valid {
                        self.authenticated = true;
                        let mut xml_buf = [0u8; MAX_XML_BODY];
                        let xml_len = auth::build_login_confirmation(
                            self.user_id,
                            &self.camera_identity,
                            &mut xml_buf,
                        )
                        .unwrap();
                        // BCEncrypt the confirmation body
                        reo_proto::encryption::bc_xor(&mut xml_buf[..xml_len], 0);
                        self.send_message(COMMAND_LOGIN, &xml_buf[..xml_len], make_status(0, 0));
                    } else {
                        let mut reject =
                            b"<body><LoginUser><result>error</result></LoginUser></body>".to_vec();
                        reo_proto::encryption::bc_xor(&mut reject, 0);
                        self.send_message(COMMAND_LOGIN, &reject, make_status(0, 0));
                    }
                }
                Err(_) => {
                    let mut reject =
                        b"<body><LoginUser><result>error</result></LoginUser></body>".to_vec();
                    reo_proto::encryption::bc_xor(&mut reject, 0);
                    self.send_message(COMMAND_LOGIN, &reject, make_status(0, 0));
                }
            }
        }
    }

    // ── Ping → Pong ──────────────────────────────────────────────────

    fn handle_ping(&mut self, request: PacketHeader) {
        self.send_message_with_offset(
            COMMAND_PING,
            &[],
            make_status(BC_CLASS_MODERN_EXT, 200),
            request.encryption_offset,
        );
    }

    // ── Stream start ack ─────────────────────────────────────────────

    fn handle_stream_start(&mut self, _body: &[u8]) {
        let xml = b"<body><Preview version=\"1.1\"><channelId>0</channelId></Preview></body>";
        self.send_modern_xml(COMMAND_STREAM, xml);
    }

    // ── Stream stop ack ──────────────────────────────────────────────

    fn handle_stream_stop(&mut self, _body: &[u8]) {
        let xml = b"<body><Preview version=\"1.1\"><channelId>0</channelId></Preview></body>";
        self.send_modern_xml(COMMAND_PREVIEW_STOP, xml);
    }

    // ── Snapshot response ────────────────────────────────────────────

    fn handle_snapshot(&mut self, _body: &[u8]) {
        let fake_jpeg = b"\xFF\xD8\xFF\xE0fake_camera_jpeg\xFF\xD9";
        self.send_binary(COMMAND_SNAP, fake_jpeg);
    }

    // ── Talk capabilities response ───────────────────────────────────

    fn handle_talk_ability(&mut self, _body: &[u8]) {
        let xml = b"<body>\
            <TalkAbility version=\"1.1\">\
                <duplexList><duplex>fullDuplex</duplex></duplexList>\
                <audioStreamModeList><audioStreamMode>speaker</audioStreamMode></audioStreamModeList>\
                <audioConfigList><audioConfig>\
                    <audioType>adpcm</audioType>\
                    <sampleRate>8000</sampleRate>\
                    <samplePrecision>16</samplePrecision>\
                    <lengthPerEncoder>320</lengthPerEncoder>\
                    <soundTrack>mono</soundTrack>\
                </audioConfig></audioConfigList>\
            </TalkAbility>\
        </body>";
        self.send_modern_xml(COMMAND_TALK_CAPABILITIES, xml);
    }

    // ── Version info response ────────────────────────────────────────

    fn handle_firmware_details(&mut self) {
        let xml = b"<body>\
            <VersionInfo version=\"1.1\">\
                <firmVer>v3.1.0_fake</firmVer>\
                <hardVer>IPC_FAKE</hardVer>\
                <name>FakeCamera</name>\
                <serial>FAKE00112233</serial>\
                <buildDay>2026-01-01</buildDay>\
                <cfgVer>v1.0</cfgVer>\
                <detail>Fake camera for testing</detail>\
            </VersionInfo>\
        </body>";
        self.send_modern_xml(COMMAND_FIRMWARE_DETAILS, xml);
    }

    // ── Media injection ──────────────────────────────────────────────

    /// Inject a binary stream message containing stream metadata
    /// followed by video frames into the outbox.
    fn inject_stream_metadata_and_video(
        &mut self,
        width: u32,
        height: u32,
        fps: u8,
        video_data: &[u8],
        is_keyframe: bool,
        microseconds: u32,
    ) {
        let mut media_payload = build_stream_metadata_bytes(width, height, fps);
        media_payload.extend(build_video_frame_bytes(
            video_data,
            is_keyframe,
            microseconds,
        ));
        self.send_binary(COMMAND_STREAM, &media_payload);
    }

    /// Inject a binary stream message containing a single video frame.
    fn inject_video_frame(&mut self, data: &[u8], is_keyframe: bool, microseconds: u32) {
        let media_payload = build_video_frame_bytes(data, is_keyframe, microseconds);
        self.send_binary(COMMAND_STREAM, &media_payload);
    }

    /// Inject a binary stream message containing an AAC audio frame.
    fn inject_audio_frame(&mut self, data: &[u8]) {
        let media_payload = build_aac_frame_bytes(data);
        self.send_binary(COMMAND_STREAM, &media_payload);
    }

    // ── Wire message builders ────────────────────────────────────────

    fn send_message(&mut self, msg_id: u32, body: &[u8], status_class: u32) {
        self.send_message_with_offset(msg_id, body, status_class, body.len() as u32);
    }

    fn send_message_with_offset(
        &mut self,
        msg_id: u32,
        body: &[u8],
        status_class: u32,
        encryption_offset: u32,
    ) {
        let has_ext =
            (status_class >> 16) == BC_CLASS_MODERN_EXT as u32 || (status_class >> 16) == 0;
        let header = PacketHeader {
            msg_id,
            body_len: body.len() as u32,
            encryption_offset,
            status_class,
            extension: if has_ext { Some(0) } else { None },
        };
        let mut hdr_buf = [0u8; HEADER_LEN_EXTENDED];
        let hdr_len = header.serialize(&mut hdr_buf);
        self.outbox.extend_from_slice(&hdr_buf[..hdr_len]);
        self.outbox.extend_from_slice(body);
    }

    fn send_modern_xml(&mut self, msg_id: u32, body: &[u8]) {
        self.send_message(msg_id, body, make_status(BC_CLASS_MODERN_EXT, 0));
    }

    fn send_binary(&mut self, msg_id: u32, body: &[u8]) {
        self.send_message(msg_id, body, make_status(BC_CLASS_LEGACY, 0));
    }
}

// ── Media frame byte builders (test helpers) ─────────────────────────

fn build_stream_metadata_bytes(width: u32, height: u32, fps: u8) -> Vec<u8> {
    let header_size: u32 = 30;
    let mut buf = Vec::new();
    buf.extend_from_slice(&MEDIA_MAGIC_INFO_V1.to_le_bytes());
    buf.extend_from_slice(&header_size.to_le_bytes());
    buf.extend_from_slice(&width.to_le_bytes());
    buf.extend_from_slice(&height.to_le_bytes());
    buf.push(0); // reserved
    buf.push(fps);
    // start timestamp
    buf.extend_from_slice(&[26, 2, 15, 10, 0, 0]);
    // end timestamp
    buf.extend_from_slice(&[26, 2, 15, 11, 0, 0]);
    while buf.len() % 8 != 0 {
        buf.push(0);
    }
    buf
}

fn build_video_frame_bytes(data: &[u8], is_keyframe: bool, microseconds: u32) -> Vec<u8> {
    let magic = if is_keyframe {
        MEDIA_MAGIC_IFRAME_BASE
    } else {
        MEDIA_MAGIC_PFRAME_BASE
    };
    let mut buf = Vec::new();
    buf.extend_from_slice(&magic.to_le_bytes());
    buf.extend_from_slice(b"H264");
    buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes()); // additional header = 0
    buf.extend_from_slice(&microseconds.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes()); // unknown
    buf.extend_from_slice(data);
    while buf.len() % 8 != 0 {
        buf.push(0);
    }
    buf
}

fn build_aac_frame_bytes(data: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&MEDIA_MAGIC_AAC.to_le_bytes());
    buf.extend_from_slice(&(data.len() as u16).to_le_bytes());
    buf.extend_from_slice(&(data.len() as u16).to_le_bytes());
    buf.extend_from_slice(data);
    while buf.len() % 8 != 0 {
        buf.push(0);
    }
    buf
}

// ── Helper: pump client→camera→client ────────────────────────────────

/// Drain all TcpSend from the client, feed them into the camera,
/// then feed the camera's response bytes back into the client.
fn pump(client: &mut BcSession, camera: &mut FakeCamera, now: Instant) {
    let mut out_buf = [0u8; 8192];
    let mut client_wire = Vec::new();

    while let Output::TcpSend { data } = client.poll_output(&mut out_buf).unwrap() {
        client_wire.extend_from_slice(data);
    }

    // 2. Feed into camera
    if !client_wire.is_empty() {
        camera.receive(&client_wire);
    }

    // 3. Feed camera response back into client
    let response = camera.take_response();
    if !response.is_empty() {
        client.handle_input(Input::TcpData(now, &response)).unwrap();
    }
}

// (No drain helper needed -- after pump(), events are the first output.)

// ══════════════════════════════════════════════════════════════════════
//  TESTS
// ══════════════════════════════════════════════════════════════════════

// ── Test: Full login handshake via FakeCamera ─────────────────────────

#[test]
fn fake_camera_full_login_handshake() {
    let now = Instant::now();
    let mut client = BcSession::default_client(now);
    let mut camera = FakeCamera::new();

    let params = LoginParams {
        username: ArrayString::try_from("admin").unwrap(),
        password: ArrayString::try_from("password123").unwrap(),
        encryption: EncryptionMode::Aes,
    };

    // Client initiates login
    client
        .handle_input(Input::Command(Command::Login(params)))
        .unwrap();
    assert_eq!(client.state(), SessionState::AwaitingNonce);

    // Pump: client legacy login → camera nonce response → client
    pump(&mut client, &mut camera, now);
    assert_eq!(client.state(), SessionState::AwaitingLoginConfirm);

    // Pump: client modern login → camera confirmation → client
    pump(&mut client, &mut camera, now);
    assert_eq!(client.state(), SessionState::Connected);
    assert!(camera.authenticated);

    // Drain the LoggedIn event
    let mut buf = [0u8; 4096];
    match client.poll_output(&mut buf).unwrap() {
        Output::Event(Event::LoggedIn(result)) => {
            assert_eq!(result.user_id, 99);
            assert_eq!(result.camera_identity.model.as_str(), "RLC-810A");
            assert_eq!(result.camera_identity.serial.as_str(), "FAKE00112233");
            assert_eq!(result.camera_identity.firmware.as_str(), "v3.1.0_fake");
            assert_eq!(result.camera_identity.channel_count, 2);
            assert_eq!(result.encryption, EncryptionMode::None);
        }
        other => panic!("expected LoggedIn, got {other:?}"),
    }
}

// ── Test: Login with wrong password ──────────────────────────────────

#[test]
fn fake_camera_login_wrong_password() {
    let now = Instant::now();
    let mut client = BcSession::default_client(now);
    let mut camera = FakeCamera::new();

    let params = LoginParams {
        username: ArrayString::try_from("admin").unwrap(),
        password: ArrayString::try_from("wrong_password").unwrap(),
        encryption: EncryptionMode::Aes,
    };

    client
        .handle_input(Input::Command(Command::Login(params)))
        .unwrap();

    // Pump legacy → nonce
    pump(&mut client, &mut camera, now);
    // Pump modern login (bad creds) → rejection
    pump(&mut client, &mut camera, now);

    assert_eq!(client.state(), SessionState::Disconnected);
    assert!(!camera.authenticated);

    let mut buf = [0u8; 4096];
    match client.poll_output(&mut buf).unwrap() {
        Output::Event(Event::LoginFailed(_)) => {}
        other => panic!("expected LoginFailed, got {other:?}"),
    }
}

// ── Test: Login then stream start/stop ───────────────────────────────

#[test]
fn fake_camera_stream_start_stop() {
    let now = Instant::now();
    let (mut client, mut camera) = login_helper(now);

    // Start stream
    let req = StreamRequest {
        channel: 0,
        handle: 0,
        stream_type: StreamType::Main,
    };
    client
        .handle_input(Input::Command(Command::StartStream(req)))
        .unwrap();
    pump(&mut client, &mut camera, now);

    let mut buf = [0u8; 4096];
    match client.poll_output(&mut buf).unwrap() {
        Output::Event(Event::StreamStarted) => {}
        other => panic!("expected StreamStarted, got {other:?}"),
    }

    // Stop stream
    let stop = StreamStop {
        channel: 0,
        handle: 0,
    };
    client
        .handle_input(Input::Command(Command::StopStream(stop)))
        .unwrap();
    pump(&mut client, &mut camera, now);

    match client.poll_output(&mut buf).unwrap() {
        Output::Event(Event::StreamStopped) => {}
        other => panic!("expected StreamStopped, got {other:?}"),
    }
}

// ── Test: Camera pushes video frames to client ───────────────────────

#[test]
fn fake_camera_video_frame_delivery() {
    let now = Instant::now();
    let (mut client, mut camera) = login_helper(now);

    // Start stream
    let req = StreamRequest {
        channel: 0,
        handle: 0,
        stream_type: StreamType::Main,
    };
    client
        .handle_input(Input::Command(Command::StartStream(req)))
        .unwrap();
    pump(&mut client, &mut camera, now);

    let mut buf = [0u8; 4096];
    // Drain StreamStarted
    match client.poll_output(&mut buf).unwrap() {
        Output::Event(Event::StreamStarted) => {}
        other => panic!("expected StreamStarted, got {other:?}"),
    }

    // Camera injects stream metadata and a keyframe
    let nal_data = b"\x00\x00\x00\x01\x67fake_sps_data";
    camera.inject_stream_metadata_and_video(1920, 1080, 30, nal_data, true, 0);
    let response = camera.take_response();
    client.handle_input(Input::TcpData(now, &response)).unwrap();

    // First event: StreamMetadata
    match client.poll_output(&mut buf).unwrap() {
        Output::Event(Event::StreamMetadata { stream_id, info }) => {
            assert_eq!(stream_id, 0);
            assert_eq!(info.width, 1920);
            assert_eq!(info.height, 1080);
            assert_eq!(info.fps, 30);
        }
        other => panic!("expected StreamMetadata, got {other:?}"),
    }

    // Second event: VideoFrame (keyframe)
    match client.poll_output(&mut buf).unwrap() {
        Output::Event(Event::VideoFrame {
            channel,
            is_keyframe,
            codec,
            data,
            timestamp,
            ..
        }) => {
            assert_eq!(channel, 0);
            assert!(is_keyframe);
            assert_eq!(codec, VideoCodec::H264);
            assert_eq!(data, nal_data);
            assert_eq!(timestamp, Duration::ZERO);
        }
        other => panic!("expected VideoFrame, got {other:?}"),
    }
}

// ── Test: Camera pushes P-frames after I-frame ───────────────────────

#[test]
fn fake_camera_iframe_then_pframes() {
    let now = Instant::now();
    let (mut client, mut camera) = login_helper(now);

    // Start stream and drain
    start_stream_helper(&mut client, &mut camera, now);

    // Keyframe
    let iframe_nal = b"\x00\x00\x00\x01\x65keyframe";
    camera.inject_video_frame(iframe_nal, true, 0);
    let data = camera.take_response();
    client.handle_input(Input::TcpData(now, &data)).unwrap();

    let mut buf = [0u8; 4096];
    match client.poll_output(&mut buf).unwrap() {
        Output::Event(Event::VideoFrame { is_keyframe, .. }) => assert!(is_keyframe),
        other => panic!("expected keyframe, got {other:?}"),
    }

    // Two P-frames
    for us in [33333, 66666] {
        let pframe_nal = b"\x00\x00\x00\x01\x41slice";
        camera.inject_video_frame(pframe_nal, false, us);
        let data = camera.take_response();
        client.handle_input(Input::TcpData(now, &data)).unwrap();

        match client.poll_output(&mut buf).unwrap() {
            Output::Event(Event::VideoFrame {
                is_keyframe,
                timestamp,
                ..
            }) => {
                assert!(!is_keyframe);
                assert_eq!(timestamp, Duration::from_micros(u64::from(us)));
            }
            other => panic!("expected P-frame, got {other:?}"),
        }
    }
}

// ── Test: Camera pushes interleaved video + audio ────────────────────

#[test]
fn fake_camera_interleaved_video_and_audio() {
    let now = Instant::now();
    let (mut client, mut camera) = login_helper(now);
    start_stream_helper(&mut client, &mut camera, now);

    // Inject video then audio
    let video_nal = b"\x00\x00\x00\x01\x65iframe";
    camera.inject_video_frame(video_nal, true, 1000);
    let audio_samples = b"aac_audio_frame_data";
    camera.inject_audio_frame(audio_samples);
    let data = camera.take_response();
    client.handle_input(Input::TcpData(now, &data)).unwrap();

    let mut buf = [0u8; 4096];

    // Video
    match client.poll_output(&mut buf).unwrap() {
        Output::Event(Event::VideoFrame {
            codec, is_keyframe, ..
        }) => {
            assert_eq!(codec, VideoCodec::H264);
            assert!(is_keyframe);
        }
        other => panic!("expected VideoFrame, got {other:?}"),
    }

    // Audio
    match client.poll_output(&mut buf).unwrap() {
        Output::Event(Event::AudioFrame {
            stream_id,
            codec,
            data,
            duration,
        }) => {
            assert_eq!(stream_id, 0);
            assert_eq!(codec, AudioCodec::Aac);
            assert_eq!(data, audio_samples);
            assert_eq!(duration, Duration::from_millis(64));
        }
        other => panic!("expected AudioFrame, got {other:?}"),
    }
}

// ── Test: Snapshot round-trip via FakeCamera ──────────────────────────

#[test]
fn fake_camera_snapshot() {
    let now = Instant::now();
    let (mut client, mut camera) = login_helper(now);

    // Send snapshot request
    client
        .handle_input(Input::Command(Command::Snapshot(SnapshotRequest {
            channel: 0,
        })))
        .unwrap();
    pump(&mut client, &mut camera, now);

    let mut buf = [0u8; 4096];
    match client.poll_output(&mut buf).unwrap() {
        Output::Event(Event::SnapshotData { data }) => {
            // Camera sends a fake JPEG
            assert!(data.starts_with(b"\xFF\xD8"));
            assert!(data.ends_with(b"\xFF\xD9"));
            assert!(data.len() > 4);
        }
        other => panic!("expected SnapshotData, got {other:?}"),
    }
}

// ── Test: Talk capabilities query via FakeCamera ─────────────────────

#[test]
fn fake_camera_talk_capabilities() {
    let now = Instant::now();
    let (mut client, mut camera) = login_helper(now);

    client
        .handle_input(Input::Command(Command::OpenTalkback { channel: 0 }))
        .unwrap();
    pump(&mut client, &mut camera, now);

    client
        .handle_input(Input::Command(Command::Talk(TalkCommand::QueryAbility {
            channel: 0,
        })))
        .unwrap();
    pump(&mut client, &mut camera, now);

    let mut buf = [0u8; 4096];
    let ability = loop {
        match client.poll_output(&mut buf).unwrap() {
            Output::Event(Event::Talk(TalkEvent::Ability(ability))) => break ability,
            Output::Event(_) => {}
            other => panic!("expected talk ability, got {other:?}"),
        }
    };
    assert_eq!(ability.duplex_modes[0].as_str(), "fullDuplex");
    assert_eq!(ability.audio_stream_modes[0].as_str(), "speaker");
    assert_eq!(ability.audio_profiles[0].sample_rate, 8000);
    assert_eq!(ability.audio_profiles[0].sample_precision, 16);
    assert_eq!(ability.audio_profiles[0].length_per_encoder, 320);
}

#[test]
fn talkback_encrypts_only_its_extension() {
    let now = Instant::now();
    let mut client = BcSession::default_client(now);
    let mut camera = FakeCamera::new();
    camera.encryption = EncryptionMode::BcEncrypt;
    do_login(&mut client, &mut camera, now);

    let mut output = [0_u8; 4096];
    assert!(matches!(
        client.poll_output(&mut output).unwrap(),
        Output::Event(Event::LoggedIn(_))
    ));

    client
        .handle_input(Input::Command(Command::OpenTalkback { channel: 0 }))
        .unwrap();
    assert!(matches!(
        client.poll_output(&mut output).unwrap(),
        Output::TcpSend { .. }
    ));

    client
        .handle_input(Input::Command(Command::Talk(TalkCommand::SendAdpcm {
            channel: 0,
            sequence: 9,
            data: vec![0, 0, 0, 0, 0],
        })))
        .unwrap();
    let wire = match client.poll_output(&mut output).unwrap() {
        Output::TcpSend { data } => data.to_vec(),
        other => panic!("expected talkback TcpSend, got {other:?}"),
    };

    let (header, header_len) = PacketHeader::parse(&wire).unwrap();
    let extension_len = header.extension.unwrap() as usize;
    let mut extension = wire[header_len..header_len + extension_len].to_vec();
    assert!(!extension.starts_with(b"<Extension"));
    reo_proto::encryption::bc_xor(&mut extension, 0);
    assert!(extension.starts_with(b"<Extension"));

    let body = &wire[header_len + extension_len..];
    assert_eq!(
        u32::from_le_bytes(body[..4].try_into().unwrap()),
        MEDIA_MAGIC_ADPCM
    );
    assert_eq!(u16::from_le_bytes(body[10..12].try_into().unwrap()), 9);
}

// ── Test: Keepalive ping/pong via FakeCamera ─────────────────────────

#[test]
fn fake_camera_keepalive_ping_pong() {
    let now = Instant::now();
    let (mut client, mut camera) = login_helper(now);

    // Manually send a ping
    client.handle_input(Input::Command(Command::Ping)).unwrap();
    pump(&mut client, &mut camera, now);

    let mut buf = [0u8; 4096];
    match client.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Pong) => {}
        other => panic!("expected Pong, got {other:?}"),
    }
}

// ── Test: Timer-driven keepalive ─────────────────────────────────────

#[test]
fn fake_camera_timer_driven_keepalive() {
    let now = Instant::now();
    let mut client = BcSession::new(
        BcSessionConfig {
            keepalive_interval: Duration::from_secs(5),
            ..BcSessionConfig::default_client()
        },
        now,
    );
    let mut camera = FakeCamera::new();

    // Perform login
    do_login(&mut client, &mut camera, now);

    // Advance time past keepalive interval
    let later = now + Duration::from_secs(6);
    client.handle_input(Input::Timeout(later)).unwrap();

    // Client should have queued a ping
    pump(&mut client, &mut camera, later);

    // Camera replied with pong, client should have a Pong event
    let mut buf = [0u8; 4096];
    match client.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Pong) => {}
        other => panic!("expected Pong from timer-driven keepalive, got {other:?}"),
    }
}

// ── Test: Firmware details query ─────────────────────────────────────

#[test]
fn fake_camera_firmware_details() {
    let now = Instant::now();
    let (mut client, mut camera) = login_helper(now);

    client
        .handle_input(Input::Command(Command::Device(
            DeviceCommand::GetFirmwareDetails,
        )))
        .unwrap();
    pump(&mut client, &mut camera, now);

    let mut buf = [0u8; 4096];
    match client.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Device(DeviceEvent::FirmwareDetails(info))) => {
            assert_eq!(info.firmware_version.as_str(), "v3.1.0_fake");
            assert_eq!(info.hardware_version.as_str(), "IPC_FAKE");
            assert_eq!(info.device_name.as_str(), "FakeCamera");
            assert_eq!(info.serial.as_str(), "FAKE00112233");
        }
        other => panic!("expected FirmwareDetails, got {other:?}"),
    }
}

// ── Test: Login + logout + re-login ──────────────────────────────────

#[test]
fn fake_camera_logout_and_relogin() {
    let now = Instant::now();
    let (mut client, mut camera) = login_helper(now);
    assert_eq!(client.state(), SessionState::Connected);

    // Logout
    client
        .handle_input(Input::Command(Command::Logout))
        .unwrap();
    assert_eq!(client.state(), SessionState::Disconnected);

    // Drain the logout TcpSend (camera doesn't need to respond)
    let mut buf = [0u8; 4096];
    loop {
        match client.poll_output(&mut buf).unwrap() {
            Output::TcpSend { .. } => continue,
            Output::Timeout(_) => break,
            other => panic!("unexpected: {other:?}"),
        }
    }

    // Re-login with fresh credentials
    let params = LoginParams {
        username: ArrayString::try_from("admin").unwrap(),
        password: ArrayString::try_from("password123").unwrap(),
        encryption: EncryptionMode::Aes,
    };
    camera.authenticated = false;

    client
        .handle_input(Input::Command(Command::Login(params)))
        .unwrap();
    assert_eq!(client.state(), SessionState::AwaitingNonce);

    pump(&mut client, &mut camera, now);
    pump(&mut client, &mut camera, now);

    assert_eq!(client.state(), SessionState::Connected);
    assert!(camera.authenticated);

    match client.poll_output(&mut buf).unwrap() {
        Output::Event(Event::LoggedIn(result)) => {
            assert_eq!(result.user_id, 99);
        }
        other => panic!("expected LoggedIn, got {other:?}"),
    }
}

// ── Test: Continuous streaming session ───────────────────────────────

#[test]
fn fake_camera_continuous_streaming_session() {
    let now = Instant::now();
    let (mut client, mut camera) = login_helper(now);
    start_stream_helper(&mut client, &mut camera, now);

    // Simulate a realistic streaming sequence:
    // info → keyframe → p-frame → p-frame → audio → p-frame
    let frames: &[(&[u8], bool, u32)] = &[
        (b"\x00\x00\x00\x01\x67sps", true, 0),
        (b"\x00\x00\x00\x01\x41slice1", false, 33333),
        (b"\x00\x00\x00\x01\x41slice2", false, 66666),
    ];

    let mut buf = [0u8; 8192];

    // Send stream metadata first
    camera.inject_stream_metadata_and_video(2560, 1440, 15, frames[0].0, frames[0].1, frames[0].2);
    let data = camera.take_response();
    client.handle_input(Input::TcpData(now, &data)).unwrap();

    // StreamMetadata
    match client.poll_output(&mut buf).unwrap() {
        Output::Event(Event::StreamMetadata { stream_id, info }) => {
            assert_eq!(stream_id, 0);
            assert_eq!(info.width, 2560);
            assert_eq!(info.height, 1440);
            assert_eq!(info.fps, 15);
        }
        other => panic!("expected StreamMetadata, got {other:?}"),
    }

    // Keyframe
    match client.poll_output(&mut buf).unwrap() {
        Output::Event(Event::VideoFrame { is_keyframe, .. }) => assert!(is_keyframe),
        other => panic!("expected keyframe, got {other:?}"),
    }

    // P-frames
    for &(nal, is_key, us) in &frames[1..] {
        camera.inject_video_frame(nal, is_key, us);
        let data = camera.take_response();
        client.handle_input(Input::TcpData(now, &data)).unwrap();

        match client.poll_output(&mut buf).unwrap() {
            Output::Event(Event::VideoFrame {
                is_keyframe,
                timestamp,
                ..
            }) => {
                assert_eq!(is_keyframe, is_key);
                assert_eq!(timestamp, Duration::from_micros(u64::from(us)));
            }
            other => panic!("expected P-frame, got {other:?}"),
        }
    }

    // Audio frame in the middle of a stream
    let audio = b"aac_sample_data_here";
    camera.inject_audio_frame(audio);
    let data = camera.take_response();
    client.handle_input(Input::TcpData(now, &data)).unwrap();

    match client.poll_output(&mut buf).unwrap() {
        Output::Event(Event::AudioFrame {
            stream_id,
            codec,
            data,
            duration,
        }) => {
            assert_eq!(stream_id, 0);
            assert_eq!(codec, AudioCodec::Aac);
            assert_eq!(data, audio);
            assert_eq!(duration, Duration::from_millis(64));
        }
        other => panic!("expected AudioFrame, got {other:?}"),
    }
}

// ── Test: Encryption downgrade via FakeCamera ────────────────────────

#[test]
fn fake_camera_encryption_downgrade() {
    let now = Instant::now();
    let mut client = BcSession::default_client(now);
    let mut camera = FakeCamera::new();
    // Camera only supports BcEncrypt
    camera.encryption = EncryptionMode::BcEncrypt;

    let params = LoginParams {
        username: ArrayString::try_from("admin").unwrap(),
        password: ArrayString::try_from("password123").unwrap(),
        encryption: EncryptionMode::FullAes, // client requests FullAes
    };

    client
        .handle_input(Input::Command(Command::Login(params)))
        .unwrap();

    pump(&mut client, &mut camera, now);
    pump(&mut client, &mut camera, now);

    assert_eq!(client.state(), SessionState::Connected);

    let mut buf = [0u8; 4096];
    match client.poll_output(&mut buf).unwrap() {
        Output::Event(Event::LoggedIn(result)) => {
            // Should have downgraded to BcEncrypt
            assert_eq!(result.encryption, EncryptionMode::BcEncrypt);
        }
        other => panic!("expected LoggedIn, got {other:?}"),
    }
}

// ── Test: Stream watchdog fires with no data from camera ─────────────

#[test]
fn fake_camera_stream_watchdog() {
    let now = Instant::now();
    let mut client = BcSession::new(
        BcSessionConfig {
            stream_watchdog_interval: Duration::from_secs(10),
            keepalive_interval: Duration::from_secs(999), // disable keepalive
            ..BcSessionConfig::default_client()
        },
        now,
    );
    let mut camera = FakeCamera::new();
    do_login(&mut client, &mut camera, now);

    // Start stream
    let req = StreamRequest {
        channel: 0,
        handle: 0,
        stream_type: StreamType::Main,
    };
    client
        .handle_input(Input::Command(Command::StartStream(req)))
        .unwrap();
    pump(&mut client, &mut camera, now);

    let mut buf = [0u8; 4096];
    // Drain StreamStarted
    match client.poll_output(&mut buf).unwrap() {
        Output::Event(Event::StreamStarted) => {}
        other => panic!("expected StreamStarted, got {other:?}"),
    }

    // Advance time past watchdog with no data from camera
    let later = now + Duration::from_secs(11);
    client.handle_input(Input::Timeout(later)).unwrap();

    match client.poll_output(&mut buf).unwrap() {
        Output::Event(Event::SessionTimeout) => {}
        other => panic!("expected SessionTimeout, got {other:?}"),
    }
}

// ══════════════════════════════════════════════════════════════════════
//  Helpers
// ══════════════════════════════════════════════════════════════════════

/// Perform a full login handshake, returning a connected client + camera.
fn login_helper(now: Instant) -> (BcSession, FakeCamera) {
    let mut client = BcSession::default_client(now);
    let mut camera = FakeCamera::new();
    do_login(&mut client, &mut camera, now);

    // Drain the LoggedIn event so callers start clean
    let mut buf = [0u8; 4096];
    match client.poll_output(&mut buf).unwrap() {
        Output::Event(Event::LoggedIn(_)) => {}
        other => panic!("login_helper: expected LoggedIn, got {other:?}"),
    }

    (client, camera)
}

/// Perform the 4-step login without draining the LoggedIn event.
fn do_login(client: &mut BcSession, camera: &mut FakeCamera, now: Instant) {
    let params = LoginParams {
        username: ArrayString::try_from("admin").unwrap(),
        password: ArrayString::try_from("password123").unwrap(),
        encryption: EncryptionMode::Aes,
    };
    client
        .handle_input(Input::Command(Command::Login(params)))
        .unwrap();

    // Legacy login → nonce
    pump(client, camera, now);
    // Modern login → confirmation
    pump(client, camera, now);

    assert_eq!(client.state(), SessionState::Connected);
}

/// Start a stream and drain the StreamStarted event.
fn start_stream_helper(client: &mut BcSession, camera: &mut FakeCamera, now: Instant) {
    let req = StreamRequest {
        channel: 0,
        handle: 0,
        stream_type: StreamType::Main,
    };
    client
        .handle_input(Input::Command(Command::StartStream(req)))
        .unwrap();
    pump(client, camera, now);

    let mut buf = [0u8; 4096];
    match client.poll_output(&mut buf).unwrap() {
        Output::Event(Event::StreamStarted) => {}
        other => panic!("start_stream_helper: expected StreamStarted, got {other:?}"),
    }
}
