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
            microseconds,
            ..
        }) => {
            assert_eq!(channel, 0);
            assert!(is_keyframe);
            assert_eq!(codec, VideoCodec::H264);
            assert_eq!(microseconds, 12345);
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
        }) => {
            assert_eq!(stream_id, 0);
            assert_eq!(codec, AudioCodec::Aac);
            assert_eq!(data, audio_data);
        }
        other => panic!("expected AudioFrame, got {other:?}"),
    }

    // No more events
    match session.poll_output(&mut buf).unwrap() {
        Output::Timeout(_) => {}
        other => panic!("expected Timeout, got {other:?}"),
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

// ── Test: Talk ability query/response round-trip ─────────────────────

#[test]
fn test_talk_capabilities_round_trip() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    // Send talk ability query
    session
        .handle_input(Input::Command(Command::QueryTalkCapabilities {
            channel: 0,
        }))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_TALK_CAPABILITIES);
    assert!(header.is_modern());

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<TalkAbility"));

    // Simulate camera response
    let response_xml = b"<body>\
        <TalkAbility version=\"1.1\">\
            <audioStreamMode>0</audioStreamMode>\
            <duplex>1</duplex>\
            <audioConfig>\
                <sampleRate>16000</sampleRate>\
                <samplePrecision>16</samplePrecision>\
                <lengthPerEncoder>640</lengthPerEncoder>\
            </audioConfig>\
        </TalkAbility>\
    </body>";

    let response = make_wire_message(
        COMMAND_TALK_CAPABILITIES,
        response_xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );

    session
        .handle_input(Input::TcpData(now, &response))
        .unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::TalkCapabilities(ability)) => {
            assert_eq!(ability.audio_stream_mode, 0);
            assert_eq!(ability.duplex_mode, 1);
            assert_eq!(ability.sample_rate, 16000);
            assert_eq!(ability.sample_precision, 16);
            assert_eq!(ability.length_per_encoder, 640);
        }
        other => panic!("expected TalkCapabilities, got {other:?}"),
    }
}

// ── Test: SendTalkData → TcpSend with correct msg_id 202 ────────────

#[test]
fn test_send_talk_data_produces_tcp_send() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let audio_payload = vec![0xAA; 320]; // 320 bytes of audio
    session
        .handle_input(Input::Command(Command::SendTalkData(audio_payload.clone())))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_TALK);
    assert!(header.is_binary());
    assert!(!header.is_extended());
    assert_eq!(header.body_len as usize, 320);

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    assert_eq!(body, &audio_payload[..]);
}

// ── Test: Talk config command ────────────────────────────────────────

#[test]
fn test_talk_config_command() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let ability = TalkCapabilities {
        audio_stream_mode: 0,
        duplex_mode: 1,
        sample_rate: 8000,
        sample_precision: 16,
        length_per_encoder: 320,
    };

    session
        .handle_input(Input::Command(Command::TalkConfig {
            channel: 0,
            ability,
        }))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_TALK_CONFIG);
    assert!(header.is_modern());

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<TalkConfig"));
    assert!(body_str.contains("<duplex>1</duplex>"));
    assert!(body_str.contains("<sampleRate>8000</sampleRate>"));
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
            microseconds,
            ..
        }) => {
            assert!(!is_keyframe, "P-frames should not be keyframes");
            assert_eq!(microseconds, 999);
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

    let result = session.handle_input(Input::Command(Command::QueryTalkCapabilities {
        channel: 0,
    }));
    assert!(matches!(result, Err(BcError::WrongRole)));

    let result = session.handle_input(Input::Command(Command::SendTalkData(vec![0; 10])));
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
            microseconds,
            ..
        }) => {
            assert!(is_keyframe);
            assert_eq!(microseconds, 0);
        }
        other => panic!("expected first VideoFrame, got {other:?}"),
    }

    // Second frame: P-frame
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::VideoFrame {
            is_keyframe,
            microseconds,
            ..
        }) => {
            assert!(!is_keyframe);
            assert_eq!(microseconds, 33333);
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
