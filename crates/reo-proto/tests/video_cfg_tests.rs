//! Phase 6 integration tests: Video & encoding configuration.
//!
//! Tests cover command → TcpSend wire format and response → Event round-trips
//! through the full BcSession state machine.

use arrayvec::ArrayString;
use reo_proto::{magic::*, video_cfg::*, *};
use std::time::Instant;

mod common;
use common::make_header_bytes;

// ── Wire message helpers ─────────────────────────────────────────────

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

fn drain_tcp_sends(session: &mut BcSession) -> Vec<u8> {
    let mut result = Vec::new();
    let mut buf = [0u8; 8192];
    while let Output::TcpSend { data } = session.poll_output(&mut buf).unwrap() {
        result.extend_from_slice(data);
    }
    result
}

// ── Test: GetVideoInput command ─────────────────────────────────────

#[test]
fn test_video_get_video_input_command() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    session
        .handle_input(Input::Command(Command::Video(
            VideoCommand::GetVideoInput { channel: 0 },
        )))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_VIDEO_INPUT_READ);
    assert!(header.is_modern());

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<VideoInput"));
    assert!(body_str.contains("<channelId>0</channelId>"));
}

// ── Test: VideoInput response → Event::Video ────────────────────────

#[test]
fn test_video_input_response() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let xml = b"<body>\
        <VideoInput version=\"1.1\">\
            <channelId>0</channelId>\
            <bright>200</bright>\
            <contrast>110</contrast>\
            <saturation>128</saturation>\
            <hue>64</hue>\
            <sharpen>80</sharpen>\
        </VideoInput>\
    </body>";

    let wire = make_wire_message(
        COMMAND_VIDEO_INPUT_READ,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Video(VideoEvent::VideoInput(s))) => {
            assert_eq!(s.brightness, 200);
            assert_eq!(s.contrast, 110);
            assert_eq!(s.hue, 64);
            assert_eq!(s.sharpness, 80);
        }
        other => panic!("expected Video(VideoInput), got {other:?}"),
    }
}

// ── Test: SetVideoInput command ─────────────────────────────────────

#[test]
fn test_video_set_video_input() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let settings = VideoInputSettings {
        channel: 0,
        brightness: 180,
        contrast: 100,
        saturation: 128,
        hue: 50,
        sharpness: 70,
    };

    session
        .handle_input(Input::Command(Command::Video(VideoCommand::SetVideoInput(
            settings,
        ))))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_VIDEO_INPUT_WRITE);

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<bright>180</bright>"));
    assert!(body_str.contains("<sharpen>70</sharpen>"));
}

// ── Test: Compression response ──────────────────────────────────────

#[test]
fn test_video_compression_response() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let xml = b"<body>\
        <Compression version=\"1.1\">\
            <channelId>0</channelId>\
            <streamType>0</streamType>\
            <width>2560</width>\
            <height>1440</height>\
            <bitRate>8192</bitRate>\
            <fps>30</fps>\
        </Compression>\
    </body>";

    let wire = make_wire_message(
        COMMAND_COMPRESSION_READ,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Video(VideoEvent::Compression(profiles))) => {
            let c = profiles.main.unwrap();
            assert_eq!(c.resolution_width, 2560);
            assert_eq!(c.resolution_height, 1440);
            assert_eq!(c.bitrate, 8192);
            assert_eq!(c.fps, 30);
        }
        other => panic!("expected Compression, got {other:?}"),
    }
}

// ── Test: SetOsd command round-trip ─────────────────────────────────

#[test]
fn test_video_set_osd_round_trip() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let osd = OsdConfig {
        channel: 0,
        enabled: true,
        pos_x: 10,
        pos_y: 20,
        name: ArrayString::try_from("Driveway").unwrap(),
    };

    session
        .handle_input(Input::Command(Command::Video(VideoCommand::SetOsd(osd))))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_OSD_WRITE);

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<name>Driveway</name>"));
    assert!(body_str.contains("<enable>1</enable>"));

    // Feed ack
    let ack = make_wire_message(
        COMMAND_OSD_WRITE,
        b"<body><OsdChannelName version=\"1.1\"></OsdChannelName></body>",
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &ack)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Video(VideoEvent::OsdAck)) => {}
        other => panic!("expected OsdAck, got {other:?}"),
    }
}

// ── Test: GetStreamCatalog ──────────────────────────────────────────

#[test]
fn test_video_stream_catalog_response() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let xml = b"<body>\
        <StreamInfoList version=\"1.1\">\
            <mainWidth>1920</mainWidth>\
            <mainHeight>1080</mainHeight>\
            <subWidth>640</subWidth>\
            <subHeight>480</subHeight>\
        </StreamInfoList>\
    </body>";

    let wire = make_wire_message(
        COMMAND_STREAM_CATALOG,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Video(VideoEvent::StreamCatalog(info))) => {
            assert_eq!(info.main_width, 1920);
            assert_eq!(info.main_height, 1080);
            assert_eq!(info.sub_width, 640);
            assert_eq!(info.sub_height, 480);
        }
        other => panic!("expected StreamCatalog, got {other:?}"),
    }
}

// ── Test: Shelter (privacy mask) round-trip ─────────────────────────

#[test]
fn test_video_shelter_round_trip() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    // Send GetShelter
    session
        .handle_input(Input::Command(Command::Video(VideoCommand::GetShelter {
            channel: 0,
        })))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, _) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_SHELTER_READ);

    // Feed response
    let xml = b"<body>\
        <Shelter version=\"1.1\">\
            <channelId>0</channelId>\
            <enable>1</enable>\
            <posX>100</posX>\
            <posY>200</posY>\
            <width>300</width>\
            <height>150</height>\
        </Shelter>\
    </body>";

    let resp = make_wire_message(
        COMMAND_SHELTER_READ,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &resp)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Video(VideoEvent::Shelter(s))) => {
            assert!(s.enabled);
            assert_eq!(s.pos_x, 100);
            assert_eq!(s.pos_y, 200);
            assert_eq!(s.width, 300);
            assert_eq!(s.height, 150);
        }
        other => panic!("expected Shelter, got {other:?}"),
    }
}

// ── Test: Video commands wrong role ─────────────────────────────────

#[test]
fn test_video_commands_wrong_role() {
    let now = Instant::now();
    let mut session = BcSession::new(BcSessionConfig::default_camera(), now);

    let result = session.handle_input(Input::Command(Command::Video(
        VideoCommand::GetVideoInput { channel: 0 },
    )));
    assert!(matches!(result, Err(BcError::WrongRole)));
}
