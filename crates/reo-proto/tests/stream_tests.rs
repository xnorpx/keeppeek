//! Phase 5 integration tests: Streaming & Two-Way Audio.
//!
//! Tests cover:
//! - Stream request/stop command → TcpSend wire format
//! - Binary stream message → VideoFrame / AudioFrame / StreamMetadata events
//! - Snapshot request/response
//! - Talk capabilities query/response round-trip
//! - Talk data send
//! - Stream watchdog behaviour
//! - Active stream counting

use reo_proto::{magic::*, media::*, *};
use std::time::{Duration, Instant};

mod common;
use common::make_header_bytes;

// ── Wire message helpers ─────────────────────────────────────────────

/// Build a complete wire message with BC header + body.
fn make_wire_message(
    msg_id: u32,
    body: &[u8],
    status_class: u32,
    extension: Option<u32>,
) -> Vec<u8> {
    let mut wire = make_header_bytes(
        msg_id,
        body.len() as u32,
        body.len() as u32,
        status_class,
        extension,
    );
    wire.extend_from_slice(body);
    wire
}

fn make_wire_message_with_header(
    msg_id: u32,
    body: &[u8],
    encryption_offset: u32,
    status_class: u32,
    extension: Option<u32>,
) -> Vec<u8> {
    let mut wire = make_header_bytes(
        msg_id,
        body.len() as u32,
        encryption_offset,
        status_class,
        extension,
    );
    wire.extend_from_slice(body);
    wire
}

/// Build a binary stream metadata frame (V1).
fn make_stream_metadata_bytes(width: u32, height: u32, fps: u8) -> Vec<u8> {
    let header_size: u32 = 30;
    let mut buf = Vec::new();
    buf.extend_from_slice(&MEDIA_MAGIC_INFO_V1.to_le_bytes());
    buf.extend_from_slice(&header_size.to_le_bytes());
    buf.extend_from_slice(&width.to_le_bytes());
    buf.extend_from_slice(&height.to_le_bytes());
    buf.push(0); // reserved byte at offset 16
    buf.push(fps); // FPS at offset 17
    // Start: year_offset=25 (2025), month=2, day=15, hour=10, min=30, sec=0
    buf.extend_from_slice(&[25, 2, 15, 10, 30, 0]);
    // End: year_offset=25, month=2, day=15, hour=11, min=30, sec=0
    buf.extend_from_slice(&[25, 2, 15, 11, 30, 0]);
    while buf.len() % 8 != 0 {
        buf.push(0);
    }
    buf
}

/// Build a binary video frame (I-frame, channel 0, H.264).
fn make_video_frame_bytes(data: &[u8], is_keyframe: bool, microseconds: u32) -> Vec<u8> {
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

/// Build a binary AAC audio frame.
fn make_aac_frame_bytes(data: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&MEDIA_MAGIC_AAC.to_le_bytes());
    buf.extend_from_slice(&(data.len() as u16).to_le_bytes());
    buf.extend_from_slice(&(data.len() as u16).to_le_bytes()); // verify
    buf.extend_from_slice(data);
    while buf.len() % 8 != 0 {
        buf.push(0);
    }
    buf
}

/// Build a binary ADPCM audio frame with the camera's four-byte subheader.
fn make_adpcm_frame_bytes(data: &[u8]) -> Vec<u8> {
    let payload_len = data.len() + 4;
    let mut buf = Vec::new();
    buf.extend_from_slice(&MEDIA_MAGIC_ADPCM.to_le_bytes());
    buf.extend_from_slice(&(payload_len as u16).to_le_bytes());
    buf.extend_from_slice(&(payload_len as u16).to_le_bytes());
    buf.extend_from_slice(&0x0100u16.to_le_bytes());
    buf.extend_from_slice(&((data.len() / 2) as u16).to_le_bytes());
    buf.extend_from_slice(data);
    while buf.len() % 8 != 0 {
        buf.push(0);
    }
    buf
}

/// Drain all TcpSend outputs (the command payload) into a single Vec.
fn drain_tcp_sends(session: &mut BcSession) -> Vec<u8> {
    let mut result = Vec::new();
    let mut buf = [0u8; 8192];
    while let Output::TcpSend { data } = session.poll_output(&mut buf).unwrap() {
        result.extend_from_slice(data);
    }
    result
}

// ── Test: Stream request XML generation ──────────────────────────────

#[test]
fn test_stream_request_command_produces_tcp_send() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let req = StreamRequest {
        channel: 0,
        handle: 0,
        stream_type: StreamType::Main,
    };
    session
        .handle_input(Input::Command(Command::StartStream(req)))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    assert!(!wire.is_empty(), "should produce TcpSend data");

    // Parse the header back
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_STREAM);
    assert!(header.is_modern());
    assert!(header.is_extended());
    assert!(!header.is_binary());

    // Check XML body
    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<Preview"));
    assert!(body_str.contains("version=\"1.1\""));
    assert!(body_str.contains("<channelId>0</channelId>"));
    assert!(body_str.contains("<handle>0</handle>"));
    assert!(body_str.contains("<streamType>mainStream</streamType>"));
}

// ── Test: Stream stop XML generation ─────────────────────────────────

#[test]
fn test_stream_stop_command_produces_tcp_send() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let stop = StreamStop {
        channel: 0,
        handle: 0,
    };
    session
        .handle_input(Input::Command(Command::StopStream(stop)))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_PREVIEW_STOP);
    assert!(header.is_modern());

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<Preview"));
    assert!(body_str.contains("<channelId>0</channelId>"));
    assert!(body_str.contains("<handle>0</handle>"));
    assert!(!body_str.contains("streamType"));
}

// ── Test: Binary stream → VideoFrame events ──────────────────────────

#[test]
fn test_binary_stream_produces_video_frame_event() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    // Build a binary stream message with one I-frame
    let nal_data = b"\x00\x00\x00\x01\x67test_iframe_data";
    let media_payload = make_video_frame_bytes(nal_data, true, 12345);
    let wire = make_wire_message(
        COMMAND_STREAM,
        &media_payload,
        make_status(BC_CLASS_LEGACY, 0),
        None,
    );

    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
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
            assert_eq!(timestamp, Duration::ZERO);
            assert!(!data.is_empty());
            assert_eq!(data, nal_data);
        }
        other => panic!("expected VideoFrame event, got {other:?}"),
    }
}

// ── Test: Binary stream with audio interleaved ───────────────────────

#[test]
fn test_binary_stream_with_interleaved_audio() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    // Build payload with video + audio frames
    let video_data = b"\x00\x00\x00\x01\x65keyframe";
    let audio_data = b"aac_audio_sample";

    let mut media_payload = make_video_frame_bytes(video_data, true, 1000);
    media_payload.extend(make_aac_frame_bytes(audio_data));

    let wire = make_wire_message(
        COMMAND_STREAM,
        &media_payload,
        make_status(BC_CLASS_LEGACY, 0),
        None,
    );

    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];

    // First event: VideoFrame
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::VideoFrame {
            is_keyframe, codec, ..
        }) => {
            assert!(is_keyframe);
            assert_eq!(codec, VideoCodec::H264);
        }
        other => panic!("expected VideoFrame, got {other:?}"),
    }

    // Second event: AudioFrame
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::AudioFrame {
            stream_id,
            codec,
            data,
            duration,
        }) => {
            assert_eq!(stream_id, 0);
            assert_eq!(codec, AudioCodec::Aac);
            assert_eq!(data, audio_data);
            assert_eq!(duration, Duration::from_millis(64));
        }
        other => panic!("expected AudioFrame, got {other:?}"),
    }

    // No more events
    match session.poll_output(&mut buf).unwrap() {
        Output::Timeout(_) => {}
        other => panic!("expected Timeout, got {other:?}"),
    }
}

#[test]
fn test_binary_stream_produces_adpcm_audio_without_frame_subheader() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let adpcm_block = b"\x34\x12\x05\x00\xAA\xBB";
    let wire = make_wire_message(
        COMMAND_STREAM,
        &make_adpcm_frame_bytes(adpcm_block),
        make_status(BC_CLASS_LEGACY, 0),
        None,
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::AudioFrame {
            codec,
            data,
            duration,
            ..
        }) => {
            assert_eq!(codec, AudioCodec::Adpcm);
            assert_eq!(data, adpcm_block);
            assert_eq!(duration, Duration::from_micros(625));
        }
        other => panic!("expected ADPCM AudioFrame, got {other:?}"),
    }
}

// ── Test: Stream metadata parsed before video ────────────────────────

#[test]
fn test_stream_metadata_then_video() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let mut media_payload = make_stream_metadata_bytes(1920, 1080, 30);
    let video_data = b"\x00\x00\x00\x01\x67sps_data";
    media_payload.extend(make_video_frame_bytes(video_data, true, 0));

    let wire = make_wire_message(
        COMMAND_STREAM,
        &media_payload,
        make_status(BC_CLASS_LEGACY, 0),
        None,
    );

    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];

    // First: StreamMetadata
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::StreamMetadata { stream_id, info }) => {
            assert_eq!(stream_id, 0);
            assert_eq!(info.width, 1920);
            assert_eq!(info.height, 1080);
            assert_eq!(info.fps, 30);
        }
        other => panic!("expected StreamMetadata, got {other:?}"),
    }

    // Second: VideoFrame
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::VideoFrame { .. }) => {}
        other => panic!("expected VideoFrame, got {other:?}"),
    }
}

// ── Test: Stream started/stopped events ──────────────────────────────

#[test]
fn test_stream_started_and_stopped_events() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    // Simulate XML stream ack response (modern, non-binary, msg_id 3)
    let xml_body = b"<body><Preview version=\"1.1\"><channelId>0</channelId></Preview></body>";
    let wire = make_wire_message(
        COMMAND_STREAM,
        xml_body,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );

    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::StreamStarted) => {}
        other => panic!("expected StreamStarted, got {other:?}"),
    }

    // Now send preview stop ack
    let stop_body = b"<body><Preview version=\"1.1\"><channelId>0</channelId></Preview></body>";
    let stop_wire = make_wire_message(
        COMMAND_PREVIEW_STOP,
        stop_body,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );

    session
        .handle_input(Input::TcpData(now, &stop_wire))
        .unwrap();

    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::StreamStopped) => {}
        other => panic!("expected StreamStopped, got {other:?}"),
    }
}

// ── Test: Watchdog fires after stream silence ────────────────────────

#[test]
fn test_stream_watchdog_after_stream_started() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    // Activate a stream via the modern ack path
    let xml_body = b"<body><Preview version=\"1.1\"><channelId>0</channelId></Preview></body>";
    let wire = make_wire_message(
        COMMAND_STREAM,
        xml_body,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 256];
    // Drain the StreamStarted event
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::StreamStarted) => {}
        other => panic!("expected StreamStarted, got {other:?}"),
    }

    // Advance time past the watchdog interval (30s default)
    let later = now + Duration::from_secs(31);
    session.handle_input(Input::Timeout(later)).unwrap();

    // Should get SessionTimeout
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::SessionTimeout) => {}
        // May also get a TcpSend from keepalive first
        Output::TcpSend { .. } => match session.poll_output(&mut buf).unwrap() {
            Output::Event(Event::SessionTimeout) => {}
            other => panic!("expected SessionTimeout after keepalive, got {other:?}"),
        },
        other => panic!("expected SessionTimeout or TcpSend, got {other:?}"),
    }
}

#[test]
fn test_ping_uses_link_type_and_matching_message_number() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);
    session.handle_input(Input::Command(Command::Ping)).unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, _) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_LINK_TYPE);
    assert_ne!(header.message_number(), 0);

    let response = make_wire_message_with_header(
        COMMAND_LINK_TYPE,
        &[],
        header.encryption_offset,
        make_status(BC_CLASS_MODERN_EXT, 200),
        Some(0),
    );
    session
        .handle_input(Input::TcpData(now, &response))
        .unwrap();

    let mut buf = [0u8; 256];
    assert!(matches!(
        session.poll_output(&mut buf).unwrap(),
        Output::Event(Event::Pong)
    ));
}

#[test]
fn test_keepalive_times_out_without_media_or_ping_replies() {
    let now = Instant::now();
    let mut session = BcSession::new(
        BcSessionConfig {
            keepalive_channel: 2,
            keepalive_interval: Duration::from_secs(1),
            ..BcSessionConfig::default_client()
        },
        now,
    );
    session.set_state(SessionState::Connected);

    let mut buf = [0u8; 256];
    for elapsed_secs in 1..=5 {
        session
            .handle_input(Input::Timeout(now + Duration::from_secs(elapsed_secs)))
            .unwrap();
        match session.poll_output(&mut buf).unwrap() {
            Output::TcpSend { data } => {
                let (header, _) = PacketHeader::parse(data).unwrap();
                assert_eq!(header.msg_id, COMMAND_LINK_TYPE);
                assert_eq!(header.channel_id(), 2);
            }
            other => panic!("expected keepalive request, got {other:?}"),
        }
    }

    session
        .handle_input(Input::Timeout(now + Duration::from_secs(6)))
        .unwrap();
    assert!(matches!(
        session.poll_output(&mut buf).unwrap(),
        Output::Event(Event::SessionTimeout)
    ));
}

#[test]
fn test_udp_keepalive_echoes_the_camera_message_header() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);
    let request_offset = 2 | (1 << 8) | (42 << 16);
    let request = make_wire_message_with_header(
        COMMAND_UDP_KEEP_ALIVE,
        &[],
        request_offset,
        make_status(BC_CLASS_MODERN_EXT, 200),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &request)).unwrap();

    let mut buf = [0u8; 256];
    match session.poll_output(&mut buf).unwrap() {
        Output::TcpSend { data } => {
            let (response, _) = PacketHeader::parse(data).unwrap();
            assert_eq!(response.msg_id, COMMAND_UDP_KEEP_ALIVE);
            assert_eq!(response.encryption_offset, request_offset);
            assert_eq!(response.response_code(), 0);
        }
        other => panic!("expected UDP keepalive echo, got {other:?}"),
    }
}

#[test]
fn test_command_outcomes_use_the_request_message_number() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);
    session
        .handle_input(Input::Command(Command::Alarm(
            AlarmCommand::StartMotionAlarm { channel: 0 },
        )))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (request, _) = PacketHeader::parse(&wire).unwrap();
    assert_ne!(request.message_number(), 0);

    let accepted = make_wire_message_with_header(
        COMMAND_START_MOTION_ALARM,
        &[],
        request.encryption_offset,
        make_status(BC_CLASS_MODERN_EXT, 200),
        Some(0),
    );
    session
        .handle_input(Input::TcpData(now, &accepted))
        .unwrap();

    let mut buf = [0u8; 256];
    assert!(matches!(
        session.poll_output(&mut buf).unwrap(),
        Output::Event(Event::Alarm(AlertEvent::MotionAlarmStarted))
    ));
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::CommandCompleted {
            msg_id,
            msg_num,
            status,
        }) => {
            assert_eq!(msg_id, COMMAND_START_MOTION_ALARM);
            assert_eq!(msg_num, request.message_number());
            assert_eq!(status, 200);
        }
        other => panic!("expected CommandCompleted, got {other:?}"),
    }

    session
        .handle_input(Input::Command(Command::Alarm(
            AlarmCommand::StartMotionAlarm { channel: 0 },
        )))
        .unwrap();
    let wire = drain_tcp_sends(&mut session);
    let (request, _) = PacketHeader::parse(&wire).unwrap();
    let rejected = make_wire_message_with_header(
        COMMAND_START_MOTION_ALARM,
        &[],
        request.encryption_offset,
        make_status(BC_CLASS_MODERN_EXT, 400),
        Some(0),
    );
    session
        .handle_input(Input::TcpData(now, &rejected))
        .unwrap();

    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::CommandFailed {
            msg_id,
            msg_num,
            status,
        }) => {
            assert_eq!(msg_id, COMMAND_START_MOTION_ALARM);
            assert_eq!(msg_num, request.message_number());
            assert_eq!(status, 400);
        }
        other => panic!("expected CommandFailed, got {other:?}"),
    }
}

#[test]
fn test_rejected_stream_request_emits_command_failure() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);
    session
        .handle_input(Input::Command(Command::SubscribeStream(
            StreamSubscription {
                channel: 0,
                stream_type: StreamType::Main,
                expected_width: 0,
                expected_height: 0,
            },
        )))
        .unwrap();

    let request = drain_tcp_sends(&mut session);
    let (request_header, _) = PacketHeader::parse(&request).unwrap();
    let rejected = make_wire_message_with_header(
        COMMAND_STREAM,
        &[],
        request_header.encryption_offset,
        make_status(BC_CLASS_MODERN_EXT, 400),
        Some(0),
    );
    session
        .handle_input(Input::TcpData(now, &rejected))
        .unwrap();

    let mut buf = [0u8; 256];
    assert!(matches!(
        session.poll_output(&mut buf).unwrap(),
        Output::Event(Event::CommandFailed {
            msg_id: COMMAND_STREAM,
            status: 400,
            ..
        })
    ));
}

// ── Test: Snapshot request + binary JPEG response ────────────────────

#[test]
fn test_snapshot_request_and_response() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    // Send snapshot request
    let req = SnapshotRequest { channel: 0 };
    session
        .handle_input(Input::Command(Command::Snapshot(req)))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_SNAP);
    assert!(header.is_modern());

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<Snap"));
    assert!(body_str.contains("<channelId>0</channelId>"));
    assert!(body_str.contains("<logicChannel>0</logicChannel>"));
    assert!(body_str.contains("<streamType>main</streamType>"));

    // Now simulate a binary JPEG response
    let jpeg_data = b"\xFF\xD8\xFF\xE0fake_jpeg_data\xFF\xD9";
    let response = make_wire_message(
        COMMAND_SNAP,
        jpeg_data,
        make_status(BC_CLASS_LEGACY, 0),
        None,
    );

    session
        .handle_input(Input::TcpData(now, &response))
        .unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::SnapshotData { data }) => {
            assert_eq!(data, jpeg_data);
        }
        other => panic!("expected SnapshotData, got {other:?}"),
    }
}

#[test]
fn test_snapshot_response_accepts_reversed_header_magic() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);
    session
        .handle_input(Input::Command(Command::Snapshot(SnapshotRequest {
            channel: 0,
        })))
        .unwrap();
    drain_tcp_sends(&mut session);

    let jpeg_data = b"\xFF\xD8\xFF\xE0reversed_header_snapshot\xFF\xD9";
    let mut response = make_wire_message(
        COMMAND_SNAP,
        jpeg_data,
        make_status(BC_CLASS_LEGACY, 0),
        None,
    );
    response[..4].copy_from_slice(&JPEG_MAGIC.to_le_bytes());
    session
        .handle_input(Input::TcpData(now, &response))
        .unwrap();

    let mut buf = [0u8; 4096];
    assert!(matches!(
        session.poll_output(&mut buf).unwrap(),
        Output::Event(Event::SnapshotData { data }) if data == jpeg_data
    ));
}

#[test]
fn test_snapshot_binary_extensions_wait_for_final_status() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);
    session
        .handle_input(Input::Command(Command::Snapshot(SnapshotRequest {
            channel: 0,
        })))
        .unwrap();
    let request = drain_tcp_sends(&mut session);
    let (request_header, _) = PacketHeader::parse(&request).unwrap();

    let jpeg_data = b"\xFF\xD8\xFF\xE0extension_snapshot\xFF\xD9";
    let metadata = format!(
        "<body><Snap version=\"1.1\"><pictureSize>{}</pictureSize></Snap></body>",
        jpeg_data.len()
    );
    let metadata_response = make_wire_message_with_header(
        COMMAND_SNAP,
        metadata.as_bytes(),
        request_header.encryption_offset,
        make_status(BC_CLASS_MODERN_EXT, 200),
        Some(0),
    );
    session
        .handle_input(Input::TcpData(now, &metadata_response))
        .unwrap();

    let extension = b"<Extension version=\"1.1\"><binaryData>1</binaryData></Extension>";
    let split_at = 8;
    let mut first_body = extension.to_vec();
    first_body.extend_from_slice(&jpeg_data[..split_at]);
    let first_chunk = make_wire_message_with_header(
        COMMAND_SNAP,
        &first_body,
        77 << 16,
        make_status(BC_CLASS_MODERN_EXT, 200),
        Some(extension.len() as u32),
    );
    session
        .handle_input(Input::TcpData(now, &first_chunk))
        .unwrap();

    let mut buf = [0u8; 4096];
    assert!(matches!(
        session.poll_output(&mut buf).unwrap(),
        Output::Timeout(_)
    ));

    let mut final_body = extension.to_vec();
    final_body.extend_from_slice(&jpeg_data[split_at..]);
    let final_chunk = make_wire_message_with_header(
        COMMAND_SNAP,
        &final_body,
        77 << 16,
        make_status(BC_CLASS_MODERN_EXT, 201),
        Some(extension.len() as u32),
    );
    session
        .handle_input(Input::TcpData(now, &final_chunk))
        .unwrap();

    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::SnapshotData { data }) => assert_eq!(data, jpeg_data),
        other => panic!("expected final SnapshotData, got {other:?}"),
    }
}

#[test]
fn test_snapshot_ignores_zero_body_ack_and_accepts_binary_alias() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);
    session
        .handle_input(Input::Command(Command::Snapshot(SnapshotRequest {
            channel: 0,
        })))
        .unwrap();
    drain_tcp_sends(&mut session);

    let acknowledged = make_wire_message_with_header(
        COMMAND_SNAP,
        &[],
        77 << 16,
        make_status(BC_CLASS_MODERN_EXT, 200),
        Some(0),
    );
    session
        .handle_input(Input::TcpData(now, &acknowledged))
        .unwrap();
    assert!(matches!(
        session.handle_input(Input::Command(Command::Snapshot(SnapshotRequest {
            channel: 0,
        }))),
        Err(BcError::Protocol("snapshot request is already in flight"))
    ));

    let jpeg_data = b"\xFF\xD8\xFF\xE0binary_alias_snapshot\xFF\xD9";
    let metadata = format!(
        "<body><Snap version=\"1.1\"><pictureSize>{}</pictureSize></Snap></body>",
        jpeg_data.len()
    );
    let metadata_response = make_wire_message_with_header(
        COMMAND_SNAP,
        metadata.as_bytes(),
        77 << 16,
        make_status(BC_CLASS_MODERN_EXT, 200),
        Some(0),
    );
    session
        .handle_input(Input::TcpData(now, &metadata_response))
        .unwrap();

    let extension = b"<Extension version=\"1.1\"><binary>1</binary></Extension>";
    let mut body = extension.to_vec();
    body.extend_from_slice(jpeg_data);
    let data_response = make_wire_message_with_header(
        COMMAND_SNAP,
        &body,
        77 << 16,
        make_status(BC_CLASS_MODERN_EXT, 201),
        Some(extension.len() as u32),
    );
    session
        .handle_input(Input::TcpData(now, &data_response))
        .unwrap();

    let mut buf = [0u8; 4096];
    assert!(matches!(
        session.poll_output(&mut buf).unwrap(),
        Output::Event(Event::SnapshotData { data }) if data == jpeg_data
    ));
    session
        .handle_input(Input::Command(Command::Snapshot(SnapshotRequest {
            channel: 0,
        })))
        .unwrap();
}

#[test]
fn test_repeated_legacy_snapshot_responses_do_not_exhaust_command_slots() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);
    let jpeg_data = b"\xFF\xD8\xFF\xD9";
    let response = make_wire_message(
        COMMAND_SNAP,
        jpeg_data,
        make_status(BC_CLASS_LEGACY, 0),
        None,
    );
    let mut buf = [0u8; 256];

    for _ in 0..129 {
        session
            .handle_input(Input::Command(Command::Snapshot(SnapshotRequest {
                channel: 0,
            })))
            .unwrap();
        drain_tcp_sends(&mut session);
        session
            .handle_input(Input::TcpData(now, &response))
            .unwrap();
        assert!(matches!(
            session.poll_output(&mut buf).unwrap(),
            Output::Event(Event::SnapshotData { data }) if data == jpeg_data
        ));
    }
}

#[test]
fn test_snapshot_rejects_overlapping_requests_and_surfaces_failure_status() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);
    session
        .handle_input(Input::Command(Command::Snapshot(SnapshotRequest {
            channel: 0,
        })))
        .unwrap();
    let request = drain_tcp_sends(&mut session);
    let (request_header, _) = PacketHeader::parse(&request).unwrap();
    assert!(matches!(
        session.handle_input(Input::Command(Command::Snapshot(SnapshotRequest {
            channel: 0,
        }))),
        Err(BcError::Protocol("snapshot request is already in flight"))
    ));

    let rejected = make_wire_message_with_header(
        COMMAND_SNAP,
        &[],
        request_header.encryption_offset,
        make_status(BC_CLASS_MODERN_EXT, 400),
        Some(0),
    );
    session
        .handle_input(Input::TcpData(now, &rejected))
        .unwrap();

    let mut buf = [0u8; 256];
    assert!(matches!(
        session.poll_output(&mut buf).unwrap(),
        Output::Event(Event::SnapshotFailed { status: 400 })
    ));
}

#[test]
fn test_modern_snapshot_metadata_and_fragmented_jpeg() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let jpeg_data = b"\xFF\xD8\xFF\xE0fragmented_snapshot\xFF\xD9";
    let metadata = format!(
        "<body><Snap version=\"1.1\"><pictureSize>{}</pictureSize></Snap></body>",
        jpeg_data.len()
    );
    let metadata_wire = make_wire_message(
        COMMAND_SNAP,
        metadata.as_bytes(),
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(metadata.len() as u32),
    );
    session
        .handle_input(Input::TcpData(now, &metadata_wire))
        .unwrap();

    let mut buf = [0u8; 4096];
    assert!(matches!(
        session.poll_output(&mut buf).unwrap(),
        Output::Timeout(_)
    ));

    let split_at = 8;
    let first_fragment = make_wire_message(
        COMMAND_SNAP,
        &jpeg_data[..split_at],
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session
        .handle_input(Input::TcpData(now, &first_fragment))
        .unwrap();
    assert!(matches!(
        session.poll_output(&mut buf).unwrap(),
        Output::Timeout(_)
    ));

    let second_fragment = make_wire_message(
        COMMAND_SNAP,
        &jpeg_data[split_at..],
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session
        .handle_input(Input::TcpData(now, &second_fragment))
        .unwrap();

    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::SnapshotData { data }) => assert_eq!(data, jpeg_data),
        other => panic!("expected complete SnapshotData, got {other:?}"),
    }
}

#[test]
fn test_modern_snapshot_metadata_with_inline_jpeg_payload() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let jpeg_data = b"\xFF\xD8\xFF\xE0inline_snapshot\xFF\xD9";
    let metadata = format!(
        "<body><Snap version=\"1.1\"><pictureSize>{}</pictureSize></Snap></body>",
        jpeg_data.len()
    );
    let mut body = metadata.into_bytes();
    let payload_offset = body.len() as u32;
    body.extend_from_slice(jpeg_data);
    let response = make_wire_message(
        COMMAND_SNAP,
        &body,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(payload_offset),
    );
    session
        .handle_input(Input::TcpData(now, &response))
        .unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::SnapshotData { data }) => assert_eq!(data, jpeg_data),
        other => panic!("expected SnapshotData, got {other:?}"),
    }
}

#[test]
fn test_modern_snapshot_accepts_battery_camera_size_limit() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let metadata = format!(
        "<body><Snap version=\"1.1\"><pictureSize>{MAX_SNAPSHOT_BYTES}</pictureSize></Snap></body>"
    );
    let response = make_wire_message(
        COMMAND_SNAP,
        metadata.as_bytes(),
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(metadata.len() as u32),
    );
    session
        .handle_input(Input::TcpData(now, &response))
        .unwrap();

    let mut buf = [0u8; 256];
    assert!(matches!(
        session.poll_output(&mut buf).unwrap(),
        Output::Timeout(_)
    ));
}

#[test]
fn test_modern_snapshot_rejects_invalid_size_metadata() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let metadata = format!(
        "<body><Snap version=\"1.1\"><pictureSize>{}</pictureSize></Snap></body>",
        MAX_SNAPSHOT_BYTES + 1
    );
    let response = make_wire_message(
        COMMAND_SNAP,
        metadata.as_bytes(),
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(metadata.len() as u32),
    );

    assert!(matches!(
        session.handle_input(Input::TcpData(now, &response)),
        Err(BcError::Protocol(
            "snapshot size is outside accepted bounds"
        ))
    ));
}

#[test]
fn test_modern_snapshot_rejects_missing_size_metadata() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let metadata = b"<body><Snap version=\"1.1\"></Snap></body>";
    let response = make_wire_message(
        COMMAND_SNAP,
        metadata,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(metadata.len() as u32),
    );

    assert!(matches!(
        session.handle_input(Input::TcpData(now, &response)),
        Err(BcError::Protocol("snapshot metadata missing pictureSize"))
    ));
}

// ── Test: External talkback command flow ─────────────────────────────

#[test]
fn test_external_talkback_command_flow() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    session
        .handle_input(Input::Command(Command::OpenTalkback { channel: 0 }))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_STREAM);
    assert!(header.is_modern());
    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<streamType>externStream</streamType>"));

    session
        .handle_input(Input::Command(Command::Talk(TalkCommand::QueryAbility {
            channel: 0,
        })))
        .unwrap();
    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_TALK_CAPABILITIES);
    assert!(header.is_extended());
    assert_ne!(header.message_number(), 0);
    let extension_len = header.extension.unwrap() as usize;
    let extension = std::str::from_utf8(&wire[hdr_len..hdr_len + extension_len]).unwrap();
    assert!(extension.starts_with("<Extension"));
    assert!(extension.contains("<channelId>0</channelId>"));

    let response_xml = b"<body>\
        <TalkAbility version=\"1.1\">\
            <duplexList><duplex>fullDuplex</duplex></duplexList>\
            <audioStreamModeList><audioStreamMode>speaker</audioStreamMode></audioStreamModeList>\
            <audioConfigList><audioConfig>\
                <audioType>adpcm</audioType>\
                <sampleRate>16000</sampleRate>\
                <samplePrecision>16</samplePrecision>\
                <lengthPerEncoder>640</lengthPerEncoder>\
                <soundTrack>mono</soundTrack>\
            </audioConfig></audioConfigList>\
        </TalkAbility>\
    </body>";

    let extension = b"<Extension version=\"1.1\"><channelId>0</channelId></Extension>";
    let mut response_body = extension.to_vec();
    response_body.extend_from_slice(response_xml);
    let response = make_wire_message(
        COMMAND_TALK_CAPABILITIES,
        &response_body,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(extension.len() as u32),
    );

    session
        .handle_input(Input::TcpData(now, &response))
        .unwrap();

    let mut buf = [0u8; 4096];
    let ability = loop {
        match session.poll_output(&mut buf).unwrap() {
            Output::Event(Event::Talk(TalkEvent::Ability(ability))) => break ability,
            Output::Event(_) => {}
            other => panic!("expected Talk ability, got {other:?}"),
        }
    };
    let config = ability.select_adpcm(0).unwrap();

    session
        .handle_input(Input::Command(Command::Talk(TalkCommand::Configure(
            config,
        ))))
        .unwrap();
    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_TALK_CONFIG);
    let extension_len = header.extension.unwrap() as usize;
    let body = std::str::from_utf8(&wire[hdr_len + extension_len..]).unwrap();
    assert!(body.contains("<TalkConfig"));
    assert!(body.contains("<audioType>adpcm</audioType>"));

    session
        .handle_input(Input::Command(Command::Talk(TalkCommand::SendAdpcm {
            channel: 0,
            sequence: 7,
            data: vec![0, 0, 0, 0, 0],
        })))
        .unwrap();
    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_TALK);
    assert!(header.is_extended());
    let extension_len = header.extension.unwrap() as usize;
    let extension = std::str::from_utf8(&wire[hdr_len..hdr_len + extension_len]).unwrap();
    assert!(extension.contains("<binaryData>1</binaryData>"));
    let body = &wire[hdr_len + extension_len..];
    assert_eq!(
        u32::from_le_bytes(body[..4].try_into().unwrap()),
        MEDIA_MAGIC_ADPCM
    );
    assert_eq!(u16::from_le_bytes(body[10..12].try_into().unwrap()), 7);
}

// ── Test: P-frame (non-keyframe) video ───────────────────────────────

#[test]
fn test_pframe_video_dispatch() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let nal_data = b"\x00\x00\x00\x01\x41pframe_data";
    let media_payload = make_video_frame_bytes(nal_data, false, 999);
    let wire = make_wire_message(
        COMMAND_STREAM,
        &media_payload,
        make_status(BC_CLASS_LEGACY, 0),
        None,
    );

    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::VideoFrame {
            is_keyframe,
            timestamp,
            ..
        }) => {
            assert!(!is_keyframe, "P-frames should not be keyframes");
            assert_eq!(timestamp, Duration::ZERO);
        }
        other => panic!("expected VideoFrame (P-frame), got {other:?}"),
    }
}

// ── Test: Stream commands rejected for camera role ───────────────────

#[test]
fn test_stream_commands_wrong_role() {
    let now = Instant::now();
    let mut session = BcSession::new(BcSessionConfig::default_camera(), now);

    let req = StreamRequest {
        channel: 0,
        handle: 0,
        stream_type: StreamType::Main,
    };
    let result = session.handle_input(Input::Command(Command::StartStream(req)));
    assert!(matches!(result, Err(BcError::WrongRole)));

    let stop = StreamStop {
        channel: 0,
        handle: 0,
    };
    let result = session.handle_input(Input::Command(Command::StopStream(stop)));
    assert!(matches!(result, Err(BcError::WrongRole)));

    let result = session.handle_input(Input::Command(Command::Snapshot(SnapshotRequest {
        channel: 0,
    })));
    assert!(matches!(result, Err(BcError::WrongRole)));

    let result = session.handle_input(Input::Command(Command::OpenTalkback { channel: 0 }));
    assert!(matches!(result, Err(BcError::WrongRole)));

    let result = session.handle_input(Input::Command(Command::Talk(TalkCommand::QueryAbility {
        channel: 0,
    })));
    assert!(matches!(result, Err(BcError::WrongRole)));
}

// ── Test: Active stream counter increments/decrements ────────────────

#[test]
fn test_active_stream_counting() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    // Start stream → active_streams = 1 (tested via watchdog behaviour)
    let xml_body = b"<body><Preview version=\"1.1\"><channelId>0</channelId></Preview></body>";
    let start_wire = make_wire_message(
        COMMAND_STREAM,
        xml_body,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session
        .handle_input(Input::TcpData(now, &start_wire))
        .unwrap();

    let mut buf = [0u8; 256];
    // Drain StreamStarted
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::StreamStarted) => {}
        other => panic!("expected StreamStarted, got {other:?}"),
    }

    // Stop stream → active_streams = 0
    let stop_body = b"<body><Preview version=\"1.1\"><channelId>0</channelId></Preview></body>";
    let stop_wire = make_wire_message(
        COMMAND_PREVIEW_STOP,
        stop_body,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session
        .handle_input(Input::TcpData(now, &stop_wire))
        .unwrap();

    // Drain StreamStopped
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::StreamStopped) => {}
        other => panic!("expected StreamStopped, got {other:?}"),
    }

    // Now timeout should NOT fire SessionTimeout (no active streams)
    let later = now + Duration::from_secs(31);
    session.handle_input(Input::Timeout(later)).unwrap();

    // We might get a keepalive TcpSend but should NOT get SessionTimeout
    loop {
        match session.poll_output(&mut buf).unwrap() {
            Output::TcpSend { .. } => continue,
            Output::Timeout(_) => break,
            Output::Event(Event::SessionTimeout) => {
                panic!("should not fire watchdog with no active streams")
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}

// ── Test: Multiple video frames in one binary message ────────────────

#[test]
fn test_multiple_video_frames_in_one_message() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let frame1_data = b"\x00\x00\x00\x01\x67sps";
    let frame2_data = b"\x00\x00\x00\x01\x41slice";

    let mut media_payload = make_video_frame_bytes(frame1_data, true, 0);
    media_payload.extend(make_video_frame_bytes(frame2_data, false, 33333));

    let wire = make_wire_message(
        COMMAND_STREAM,
        &media_payload,
        make_status(BC_CLASS_LEGACY, 0),
        None,
    );

    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];

    // First frame: keyframe
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::VideoFrame {
            is_keyframe,
            timestamp,
            ..
        }) => {
            assert!(is_keyframe);
            assert_eq!(timestamp, Duration::ZERO);
        }
        other => panic!("expected first VideoFrame, got {other:?}"),
    }

    // Second frame: P-frame
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::VideoFrame {
            is_keyframe,
            timestamp,
            ..
        }) => {
            assert!(!is_keyframe);
            assert_eq!(timestamp, Duration::from_micros(33_333));
        }
        other => panic!("expected second VideoFrame, got {other:?}"),
    }
}

// ── Test: Sub stream type ────────────────────────────────────────────

#[test]
fn test_stream_request_sub_stream() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let req = StreamRequest {
        channel: 1,
        handle: 42,
        stream_type: StreamType::Sub,
    };
    session
        .handle_input(Input::Command(Command::StartStream(req)))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<channelId>1</channelId>"));
    assert!(body_str.contains("<handle>42</handle>"));
    assert!(body_str.contains("<streamType>subStream</streamType>"));
}
