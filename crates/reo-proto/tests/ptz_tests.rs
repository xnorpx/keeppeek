//! Phase 7 integration tests: PTZ (Pan-Tilt-Zoom) control.
//!
//! Tests cover command → TcpSend wire format and response → Event round-trips
//! through the full BcSession state machine.

use arrayvec::ArrayString;
use reo_proto::{magic::*, ptz::*, *};
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

// ── Test: PTZ Move command ───────────────────────────────────────────

#[test]
fn test_ptz_move_left_command() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    session
        .handle_input(Input::Command(Command::Ptz(PtzCommand::Move {
            channel: 0,
            direction: PtzDirection::Left,
            speed: 32,
        })))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_PTZ);
    assert!(header.is_modern());

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<PtzControl"));
    assert!(body_str.contains("<command>left</command>"));
    assert!(body_str.contains("<speed>32</speed>"));
}

// ── Test: PTZ Stop command ───────────────────────────────────────────

#[test]
fn test_ptz_stop_command() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    session
        .handle_input(Input::Command(Command::Ptz(PtzCommand::Stop {
            channel: 0,
        })))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_PTZ);

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<command>stop</command>"));
    assert!(body_str.contains("<speed>0</speed>"));
}

// ── Test: PTZ Move response → Event::Ptz ────────────────────────────

#[test]
fn test_ptz_move_ack_response() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let xml = b"<body><PtzControl version=\"1.1\"></PtzControl></body>";
    let wire = make_wire_message(
        COMMAND_PTZ,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Ptz(PtzEvent::MoveAck)) => {}
        other => panic!("expected Ptz(MoveAck), got {other:?}"),
    }
}

// ── Test: PresetList command and response ────────────────────────────

#[test]
fn test_ptz_preset_list_round_trip() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    session
        .handle_input(Input::Command(Command::Ptz(PtzCommand::PresetList {
            channel: 0,
        })))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, _) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_PTZ_PRESET_LIST);

    // Feed response
    let xml = b"<body>\
        <PtzPresetList version=\"1.1\">\
            <presetId>1</presetId>\
            <name>Front Gate</name>\
            <presetId>2</presetId>\
            <name>Backyard</name>\
        </PtzPresetList>\
    </body>";

    let resp = make_wire_message(
        COMMAND_PTZ_PRESET_LIST,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &resp)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Ptz(PtzEvent::PresetList(list))) => {
            assert_eq!(list.len(), 2);
            assert_eq!(list[0].id, 1);
            assert_eq!(list[0].name.as_str(), "Front Gate");
            assert_eq!(list[1].id, 2);
            assert_eq!(list[1].name.as_str(), "Backyard");
        }
        other => panic!("expected Ptz(PresetList), got {other:?}"),
    }
}

// ── Test: PresetGoto command ─────────────────────────────────────────

#[test]
fn test_ptz_preset_goto_command() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    session
        .handle_input(Input::Command(Command::Ptz(PtzCommand::PresetGoto {
            channel: 0,
            preset_id: 5,
        })))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_PTZ_PRESET);

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<command>goto</command>"));
    assert!(body_str.contains("<presetId>5</presetId>"));
}

// ── Test: PresetSave command ─────────────────────────────────────────

#[test]
fn test_ptz_preset_save_command() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    session
        .handle_input(Input::Command(Command::Ptz(PtzCommand::PresetSave {
            channel: 0,
            preset_id: 3,
            name: ArrayString::try_from("Gate View").unwrap(),
        })))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_PTZ_PRESET);

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<command>save</command>"));
    assert!(body_str.contains("<name>Gate View</name>"));
}

// ── Test: GetZoomFocus command and response ──────────────────────────

#[test]
fn test_ptz_zoom_focus_round_trip() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    session
        .handle_input(Input::Command(Command::Ptz(PtzCommand::GetZoomFocus {
            channel: 0,
        })))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, _) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_PTZ_ZOOM_FOCUS);

    // Feed response
    let xml = b"<body>\
        <PtzZoomFocus version=\"1.1\">\
            <zoom>500</zoom>\
            <focus>300</focus>\
        </PtzZoomFocus>\
    </body>";

    let resp = make_wire_message(
        COMMAND_PTZ_ZOOM_FOCUS,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &resp)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Ptz(PtzEvent::ZoomFocus(info))) => {
            assert_eq!(info.zoom, 500);
            assert_eq!(info.focus, 300);
        }
        other => panic!("expected Ptz(ZoomFocus), got {other:?}"),
    }
}

// ── Test: StartZoomFocus command ─────────────────────────────────────

#[test]
fn test_ptz_start_zoom_focus() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    session
        .handle_input(Input::Command(Command::Ptz(PtzCommand::StartZoomFocus {
            channel: 0,
            operation: ZoomFocusOp::ZoomIn,
        })))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_START_ZOOM_FOCUS);

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<command>zoomIn</command>"));
}

// ── Test: SetGuard command and response ──────────────────────────────

#[test]
fn test_ptz_guard_round_trip() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let cfg = PtzGuardConfig {
        channel: 0,
        enabled: true,
        preset_id: 1,
        wait_time: 30,
    };

    session
        .handle_input(Input::Command(Command::Ptz(PtzCommand::SetGuard(cfg))))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_PTZ_GUARD);

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<enable>1</enable>"));
    assert!(body_str.contains("<presetId>1</presetId>"));
    assert!(body_str.contains("<waitTime>30</waitTime>"));

    // Feed guard response
    let xml = b"<body>\
        <PtzGuard version=\"1.1\">\
            <channelId>0</channelId>\
            <enable>1</enable>\
            <presetId>1</presetId>\
            <waitTime>30</waitTime>\
        </PtzGuard>\
    </body>";

    let resp = make_wire_message(
        COMMAND_PTZ_GUARD,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &resp)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Ptz(PtzEvent::Guard(g))) => {
            assert!(g.enabled);
            assert_eq!(g.preset_id, 1);
            assert_eq!(g.wait_time, 30);
        }
        other => panic!("expected Ptz(Guard), got {other:?}"),
    }
}

// ── Test: PTZ commands wrong role ────────────────────────────────────

#[test]
fn test_ptz_commands_wrong_role() {
    let now = Instant::now();
    let mut session = BcSession::new(BcSessionConfig::default_camera(), now);

    let result = session.handle_input(Input::Command(Command::Ptz(PtzCommand::Move {
        channel: 0,
        direction: PtzDirection::Left,
        speed: 10,
    })));
    assert!(matches!(result, Err(BcError::WrongRole)));
}
