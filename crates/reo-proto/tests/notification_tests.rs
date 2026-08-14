//! Phase 8 integration tests: Notifications & output devices.
//!
//! Tests cover command → TcpSend wire format and response → Event round-trips
//! through the full BcSession state machine.

use arrayvec::ArrayString;
use reo_proto::{magic::*, notification::*, *};
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

// ── Test: Email config round-trip ───────────────────────────────────

#[test]
fn test_notification_email_round_trip() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let cfg = EmailConfig {
        channel: 0,
        enabled: true,
        smtp_server: ArrayString::try_from("smtp.example.com").unwrap(),
        smtp_port: 587,
        sender: ArrayString::try_from("cam@example.com").unwrap(),
        ssl: true,
    };

    session
        .handle_input(Input::Command(Command::Notification(
            NotificationCommand::SetEmail(cfg),
        )))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_EMAIL_WRITE);

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<smtpServer>smtp.example.com</smtpServer>"));
    assert!(body_str.contains("<smtpPort>587</smtpPort>"));

    // Feed ack
    let ack = make_wire_message(
        COMMAND_EMAIL_WRITE,
        b"<body><Email version=\"1.1\"></Email></body>",
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &ack)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Notification(NotificationEvent::EmailAck)) => {}
        other => panic!("expected Notification(EmailAck), got {other:?}"),
    }
}

// ── Test: Email read response ───────────────────────────────────────

#[test]
fn test_notification_email_read_response() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let xml = b"<body>\
        <Email version=\"1.1\">\
            <channelId>0</channelId>\
            <enable>1</enable>\
            <smtpServer>smtp.example.com</smtpServer>\
            <smtpPort>587</smtpPort>\
            <sender>cam@example.com</sender>\
            <ssl>1</ssl>\
        </Email>\
    </body>";

    let wire = make_wire_message(
        COMMAND_EMAIL_READ,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Notification(NotificationEvent::Email(cfg))) => {
            assert!(cfg.enabled);
            assert_eq!(cfg.smtp_server.as_str(), "smtp.example.com");
            assert_eq!(cfg.smtp_port, 587);
            assert!(cfg.ssl);
        }
        other => panic!("expected Notification(Email), got {other:?}"),
    }
}

// ── Test: Email test ────────────────────────────────────────────────

#[test]
fn test_notification_email_test() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    session
        .handle_input(Input::Command(Command::Notification(
            NotificationCommand::TestEmail,
        )))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, _) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_EMAIL_TEST);

    // Feed response
    let xml = b"<body>\
        <EmailTest version=\"1.1\">\
            <result>ok</result>\
        </EmailTest>\
    </body>";

    let resp = make_wire_message(
        COMMAND_EMAIL_TEST,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &resp)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Notification(NotificationEvent::EmailTestResult { success })) => {
            assert!(success);
        }
        other => panic!("expected Notification(EmailTestResult), got {other:?}"),
    }
}

// ── Test: LED state round-trip ──────────────────────────────────────

#[test]
fn test_notification_led_state_round_trip() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let cfg = LedStateConfig {
        channel: 0,
        enabled: true,
        state: ArrayString::try_from("auto").unwrap(),
    };

    session
        .handle_input(Input::Command(Command::Notification(
            NotificationCommand::SetLedState(cfg),
        )))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_LED_WRITE);

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<enable>1</enable>"));
    assert!(body_str.contains("<state>auto</state>"));

    // Feed response
    let xml = b"<body>\
        <LedState version=\"1.1\">\
            <channelId>0</channelId>\
            <enable>1</enable>\
            <state>auto</state>\
        </LedState>\
    </body>";

    let resp = make_wire_message(
        COMMAND_LED_READ,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &resp)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Notification(NotificationEvent::LedState(cfg))) => {
            assert!(cfg.enabled);
            assert_eq!(cfg.state.as_str(), "auto");
        }
        other => panic!("expected Notification(LedState), got {other:?}"),
    }
}

// ── Test: Battery info response ─────────────────────────────────────

#[test]
fn test_notification_battery_info() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let xml = b"<body>\
        <BatteryInfo version=\"1.1\">\
            <channelId>0</channelId>\
            <capacity>85</capacity>\
            <temperature>25</temperature>\
            <charging>1</charging>\
        </BatteryInfo>\
    </body>";

    let wire = make_wire_message(
        COMMAND_BATTERY_INFO,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Notification(NotificationEvent::BatteryInfo(data))) => {
            assert_eq!(data.capacity, 85);
            assert_eq!(data.temperature, 25);
            assert!(data.charging);
        }
        other => panic!("expected Notification(BatteryInfo), got {other:?}"),
    }
}

// ── Test: Floodlight on/off round-trip ──────────────────────────────

#[test]
fn test_notification_floodlight_round_trip() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let cfg = ManualLightState {
        channel: 0,
        enabled: true,
    };

    session
        .handle_input(Input::Command(Command::Notification(
            NotificationCommand::SetFloodlight(cfg),
        )))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_FLOODLIGHT);

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<enable>1</enable>"));

    // Feed ack
    let ack = make_wire_message(
        COMMAND_FLOODLIGHT,
        b"<body><Floodlight version=\"1.1\"></Floodlight></body>",
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &ack)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Notification(NotificationEvent::FloodlightAck)) => {}
        other => panic!("expected Notification(FloodlightAck), got {other:?}"),
    }
}

// ── Test: FloodlightTask config ─────────────────────────────────────

#[test]
fn test_notification_floodlight_task() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let xml = b"<body>\
        <FloodlightTask version=\"1.1\">\
            <channelId>0</channelId>\
            <enable>1</enable>\
            <brightness>80</brightness>\
        </FloodlightTask>\
    </body>";

    let wire = make_wire_message(
        COMMAND_FLOODLIGHT_TASK_READ,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Notification(NotificationEvent::FloodlightTask(cfg))) => {
            assert!(cfg.enabled);
            assert_eq!(cfg.brightness, 80);
        }
        other => panic!("expected Notification(FloodlightTask), got {other:?}"),
    }
}

// ── Test: Siren control command ─────────────────────────────────────

#[test]
fn test_notification_siren_control() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    session
        .handle_input(Input::Command(Command::Notification(
            NotificationCommand::SirenControl {
                channel: 0,
                enabled: true,
            },
        )))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_SIREN_CONTROL);

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<SirenControl"));
    assert!(body_str.contains("<enable>1</enable>"));

    // Feed ack
    let ack = make_wire_message(
        COMMAND_SIREN_CONTROL,
        b"<body><SirenControl version=\"1.1\"></SirenControl></body>",
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &ack)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Notification(NotificationEvent::SirenControlAck)) => {}
        other => panic!("expected Notification(SirenControlAck), got {other:?}"),
    }
}

// ── Test: Battery list response ─────────────────────────────────────

#[test]
fn test_notification_battery_list() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let xml = b"<body>\
        <BatteryList version=\"1.1\">\
            <channelId>0</channelId>\
            <capacity>85</capacity>\
            <temperature>25</temperature>\
            <charging>1</charging>\
            <channelId>1</channelId>\
            <capacity>42</capacity>\
            <temperature>30</temperature>\
            <charging>0</charging>\
        </BatteryList>\
    </body>";

    let wire = make_wire_message(
        COMMAND_BATTERY_LIST,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Notification(NotificationEvent::BatteryList(list))) => {
            assert_eq!(list.len(), 2);
            assert_eq!(list[0].capacity, 85);
            assert!(list[0].charging);
            assert_eq!(list[1].capacity, 42);
            assert!(!list[1].charging);
        }
        other => panic!("expected Notification(BatteryList), got {other:?}"),
    }
}

// ── Test: Notification commands wrong role ───────────────────────────

#[test]
fn test_notification_commands_wrong_role() {
    let now = Instant::now();
    let mut session = BcSession::new(BcSessionConfig::default_camera(), now);

    let result = session.handle_input(Input::Command(Command::Notification(
        NotificationCommand::TestEmail,
    )));
    assert!(matches!(result, Err(BcError::WrongRole)));
}

// ── Test: Push info response ────────────────────────────────────────

#[test]
fn test_notification_push_info() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let xml = b"<body>\
        <PushInfo version=\"1.1\">\
            <channelId>0</channelId>\
            <enable>1</enable>\
        </PushInfo>\
    </body>";

    let wire = make_wire_message(
        COMMAND_PUSH_INFO,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Notification(NotificationEvent::PushInfo(data))) => {
            assert!(data.enabled);
        }
        other => panic!("expected Notification(PushInfo), got {other:?}"),
    }
}

// ── Test: Audio play info response ──────────────────────────────────

#[test]
fn test_notification_audio_play_info() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let xml = b"<body>\
        <AudioPlayInfo version=\"1.1\">\
            <channelId>0</channelId>\
            <enable>1</enable>\
        </AudioPlayInfo>\
    </body>";

    let wire = make_wire_message(
        COMMAND_AUDIO_PLAY_INFO,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Notification(NotificationEvent::AudioPlayInfo(data))) => {
            assert!(data.enabled);
        }
        other => panic!("expected Notification(AudioPlayInfo), got {other:?}"),
    }
}
