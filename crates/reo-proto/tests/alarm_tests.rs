//! Phase 7 integration tests: Alarms & detection.
//!
//! Tests cover command → TcpSend wire format and response → Event round-trips
//! through the full BcSession state machine.

use reo_proto::{alarm::*, magic::*, *};
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

// ── Test: StartMotionAlarm command ───────────────────────────────────

#[test]
fn test_alarm_start_motion_alarm_command() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    session
        .handle_input(Input::Command(Command::Alarm(
            AlarmCommand::StartMotionAlarm { channel: 0 },
        )))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_START_MOTION_ALARM);
    assert!(header.is_modern());

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<StartMotionAlarm"));
    assert!(body_str.contains("<channelId>0</channelId>"));
}

// ── Test: StartMotionAlarm ack response ──────────────────────────────

#[test]
fn test_alarm_motion_alarm_started_response() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let xml = b"<body><StartMotionAlarm version=\"1.1\"></StartMotionAlarm></body>";
    let wire = make_wire_message(
        COMMAND_START_MOTION_ALARM,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Alarm(AlertEvent::MotionAlarmStarted)) => {}
        other => panic!("expected Alarm(MotionAlarmStarted), got {other:?}"),
    }
}

// ── Test: Unsolicited AlarmEventList push ────────────────────────────

#[test]
fn test_alarm_event_list_unsolicited() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let xml = b"<body>\
        <AlarmEventList version=\"1.1\">\
            <channelId>0</channelId>\
            <alarmType>motion</alarmType>\
            <status>1</status>\
        </AlarmEventList>\
    </body>";

    let wire = make_wire_message(
        COMMAND_ALARM_EVENT_LIST,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Alarm(AlertEvent::AlarmEventList(data))) => {
            assert_eq!(data.channel, 0);
            assert_eq!(data.alarm_type.as_str(), "motion");
            assert!(data.status);
        }
        other => panic!("expected Alarm(AlarmEventList), got {other:?}"),
    }
}

// ── Test: MotionDetect round-trip ────────────────────────────────────

#[test]
fn test_alarm_motion_detect_round_trip() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    // Send GetMotionDetect
    session
        .handle_input(Input::Command(Command::Alarm(
            AlarmCommand::GetMotionDetect { channel: 0 },
        )))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, _) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_MOTION_DETECT_READ);

    // Feed response
    let xml = b"<body>\
        <MotionDetect version=\"1.1\">\
            <channelId>0</channelId>\
            <enable>1</enable>\
            <sensitivity>60</sensitivity>\
        </MotionDetect>\
    </body>";

    let resp = make_wire_message(
        COMMAND_MOTION_DETECT_READ,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &resp)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Alarm(AlertEvent::MotionDetect(cfg))) => {
            assert!(cfg.enabled);
            assert_eq!(cfg.sensitivity, 60);
        }
        other => panic!("expected Alarm(MotionDetect), got {other:?}"),
    }
}

// ── Test: SetMotionDetect command ────────────────────────────────────

#[test]
fn test_alarm_set_motion_detect() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let cfg = MotionDetectConfig {
        channel: 0,
        enabled: true,
        sensitivity: 50,
    };

    session
        .handle_input(Input::Command(Command::Alarm(
            AlarmCommand::SetMotionDetect(cfg),
        )))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_MOTION_DETECT_WRITE);

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<enable>1</enable>"));
    assert!(body_str.contains("<sensitivity>50</sensitivity>"));

    // Feed ack
    let ack = make_wire_message(
        COMMAND_MOTION_DETECT_WRITE,
        b"<body><MotionDetect version=\"1.1\"></MotionDetect></body>",
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &ack)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Alarm(AlertEvent::MotionDetectAck)) => {}
        other => panic!("expected Alarm(MotionDetectAck), got {other:?}"),
    }
}

// ── Test: AI alarm round-trip ────────────────────────────────────────

#[test]
fn test_alarm_ai_alarm_round_trip() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    // Send SetAiAlarm
    let cfg = AiAlarmConfig {
        channel: 0,
        person: true,
        vehicle: false,
        dog_cat: true,
        face: false,
        package: true,
    };

    session
        .handle_input(Input::Command(Command::Alarm(AlarmCommand::SetAiAlarm(
            cfg,
        ))))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_AI_ALARM_WRITE);

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<person>1</person>"));
    assert!(body_str.contains("<vehicle>0</vehicle>"));
    assert!(body_str.contains("<dogCat>1</dogCat>"));

    // Feed ack
    let ack = make_wire_message(
        COMMAND_AI_ALARM_WRITE,
        b"<body><AiAlarm version=\"1.1\"></AiAlarm></body>",
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &ack)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Alarm(AlertEvent::AiAlarmAck)) => {}
        other => panic!("expected Alarm(AiAlarmAck), got {other:?}"),
    }
}

// ── Test: AI alarm read response ─────────────────────────────────────

#[test]
fn test_alarm_ai_alarm_read_response() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let xml = b"<body>\
        <AiAlarm version=\"1.1\">\
            <channelId>0</channelId>\
            <person>1</person>\
            <vehicle>1</vehicle>\
            <dogCat>0</dogCat>\
            <face>1</face>\
            <package>0</package>\
        </AiAlarm>\
    </body>";

    let wire = make_wire_message(
        COMMAND_AI_ALARM_READ,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Alarm(AlertEvent::AiAlarm(cfg))) => {
            assert!(cfg.person);
            assert!(cfg.vehicle);
            assert!(!cfg.dog_cat);
            assert!(cfg.face);
            assert!(!cfg.package);
        }
        other => panic!("expected Alarm(AiAlarm), got {other:?}"),
    }
}

// ── Test: PIR round-trip ─────────────────────────────────────────────

#[test]
fn test_alarm_pir_round_trip() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    session
        .handle_input(Input::Command(Command::Alarm(AlarmCommand::GetPir {
            channel: 0,
        })))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, _) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_PIR_READ);

    // Feed response
    let xml = b"<body>\
        <PirInfo version=\"1.1\">\
            <channelId>0</channelId>\
            <enable>1</enable>\
            <sensitivity>90</sensitivity>\
        </PirInfo>\
    </body>";

    let resp = make_wire_message(
        COMMAND_PIR_READ,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &resp)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Alarm(AlertEvent::Pir(cfg))) => {
            assert!(cfg.enabled);
            assert_eq!(cfg.sensitivity, 90);
        }
        other => panic!("expected Alarm(Pir), got {other:?}"),
    }
}

// ── Test: Unsolicited CoordinateInfo push ────────────────────────────

#[test]
fn test_alarm_coordinate_info_unsolicited() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let xml = b"<body>\
        <CoordinateInfo version=\"1.1\">\
            <channelId>0</channelId>\
            <x>320</x>\
            <y>240</y>\
        </CoordinateInfo>\
    </body>";

    let wire = make_wire_message(
        COMMAND_COORDINATE_INFO,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Alarm(AlertEvent::CoordinateInfo(data))) => {
            assert_eq!(data.x, 320);
            assert_eq!(data.y, 240);
        }
        other => panic!("expected Alarm(CoordinateInfo), got {other:?}"),
    }
}

// ── Test: Alarm commands wrong role ──────────────────────────────────

#[test]
fn test_alarm_commands_wrong_role() {
    let now = Instant::now();
    let mut session = BcSession::new(BcSessionConfig::default_camera(), now);

    let result = session.handle_input(Input::Command(Command::Alarm(
        AlarmCommand::StartMotionAlarm { channel: 0 },
    )));
    assert!(matches!(result, Err(BcError::WrongRole)));
}

// ── Test: AiCfg response ─────────────────────────────────────────────

#[test]
fn test_alarm_ai_cfg_response() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let xml = b"<body>\
        <AiCfg version=\"1.1\">\
            <channelId>0</channelId>\
            <trackEnable>1</trackEnable>\
            <sensitivity>80</sensitivity>\
        </AiCfg>\
    </body>";

    let wire = make_wire_message(
        COMMAND_AI_CFG_READ,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Alarm(AlertEvent::AiCfg(data))) => {
            assert!(data.track_enabled);
            assert_eq!(data.sensitivity, 80);
        }
        other => panic!("expected Alarm(AiCfg), got {other:?}"),
    }
}

// ── Test: RF alarm response ──────────────────────────────────────────

#[test]
fn test_alarm_rf_alarm_response() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let xml = b"<body>\
        <RfAlarm version=\"1.1\">\
            <channelId>0</channelId>\
            <enable>1</enable>\
        </RfAlarm>\
    </body>";

    let wire = make_wire_message(
        COMMAND_RF_ALARM,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Alarm(AlertEvent::RfAlarm(cfg))) => {
            assert!(cfg.enabled);
        }
        other => panic!("expected Alarm(RfAlarm), got {other:?}"),
    }
}
