//! Phase 6 integration tests: Device & system queries.
//!
//! Tests cover command → TcpSend wire format and response → Event round-trips
//! through the full BcSession state machine.

use reo_proto::{device::*, magic::*, *};
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

// ── Test: GetFirmwareDetails command → TcpSend ──────────────────────

#[test]
fn test_device_get_version_info_command() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    session
        .handle_input(Input::Command(Command::Device(
            DeviceCommand::GetFirmwareDetails,
        )))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_FIRMWARE_DETAILS);
    assert!(header.is_modern());
    assert!(header.is_extended());

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<VersionInfo"));
    assert!(body_str.contains("version=\"1.1\""));
}

// ── Test: FirmwareDetails response → Event::Device ──────────────────

#[test]
fn test_device_version_info_response() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let xml = b"<body>\
        <VersionInfo version=\"1.1\">\
            <firmVer>v3.1.0</firmVer>\
            <hardVer>IPC_5678</hardVer>\
            <name>FrontDoor</name>\
            <serial>XYZ789</serial>\
            <buildDay>2025-06-01</buildDay>\
            <cfgVer>v2.0</cfgVer>\
            <detail>details here</detail>\
        </VersionInfo>\
    </body>";

    let wire = make_wire_message(
        COMMAND_FIRMWARE_DETAILS,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );

    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Device(DeviceEvent::FirmwareDetails(info))) => {
            assert_eq!(info.firmware_version.as_str(), "v3.1.0");
            assert_eq!(info.hardware_version.as_str(), "IPC_5678");
            assert_eq!(info.device_name.as_str(), "FrontDoor");
            assert_eq!(info.serial.as_str(), "XYZ789");
        }
        other => panic!("expected Device(FirmwareDetails), got {other:?}"),
    }
}

// ── Test: GetAbilitySupport round-trip ──────────────────────────────

#[test]
fn test_device_ability_support_round_trip() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    session
        .handle_input(Input::Command(Command::Device(
            DeviceCommand::GetAbilitySupport,
        )))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, _) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_ABILITY_SUPPORT);

    // Feed response
    let xml = b"<body>\
        <AbilitySupport version=\"1.1\">\
            <supportPtz>1</supportPtz>\
            <supportTalk>0</supportTalk>\
            <supportRecord>1</supportRecord>\
            <supportAlarm>1</supportAlarm>\
            <supportWifi>0</supportWifi>\
            <supportCloud>1</supportCloud>\
        </AbilitySupport>\
    </body>";

    let resp = make_wire_message(
        COMMAND_ABILITY_SUPPORT,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &resp)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Device(DeviceEvent::AbilitySupport(a))) => {
            assert!(a.support_ptz);
            assert!(!a.support_talk);
            assert!(a.support_record);
            assert!(a.support_cloud);
        }
        other => panic!("expected AbilitySupport, got {other:?}"),
    }
}

// ── Test: SetTimeCfg command ─────────────────────────────────────────

#[test]
fn test_device_set_time_cfg() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let cfg = TimeCfg {
        year: 2026,
        month: 2,
        day: 15,
        hour: 12,
        minute: 0,
        second: 0,
    };

    session
        .handle_input(Input::Command(Command::Device(DeviceCommand::SetTimeCfg(
            cfg,
        ))))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_TIME_CFG);

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<year>2026</year>"));
    assert!(body_str.contains("<month>2</month>"));
}

// ── Test: Reboot command ─────────────────────────────────────────────

#[test]
fn test_device_reboot_command_and_ack() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    session
        .handle_input(Input::Command(Command::Device(DeviceCommand::Reboot)))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, _) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_REBOOT);

    // Feed ack
    let xml = b"<body><Reboot version=\"1.1\"></Reboot></body>";
    let resp = make_wire_message(
        COMMAND_REBOOT,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &resp)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Device(DeviceEvent::RebootAck)) => {}
        other => panic!("expected RebootAck, got {other:?}"),
    }
}

// ── Test: SystemSettings response ───────────────────────────────────

#[test]
fn test_device_system_general_response() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let xml = b"<body>\
        <SystemGeneral version=\"1.1\">\
            <timeZone>3600</timeZone>\
            <year>2026</year>\
            <month>1</month>\
            <day>10</day>\
            <hour>8</hour>\
            <minute>45</minute>\
            <second>30</second>\
            <deviceName>Backyard</deviceName>\
            <language>English</language>\
        </SystemGeneral>\
    </body>";

    let wire = make_wire_message(
        COMMAND_SYSTEM_SETTINGS,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Device(DeviceEvent::SystemSettings(sg))) => {
            assert_eq!(sg.timezone, 3600);
            assert_eq!(sg.year, 2026);
            assert_eq!(sg.device_name.as_str(), "Backyard");
        }
        other => panic!("expected SystemSettings, got {other:?}"),
    }
}

// ── Test: Device commands wrong role ────────────────────────────────

#[test]
fn test_device_commands_wrong_role() {
    let now = Instant::now();
    let mut session = BcSession::new(BcSessionConfig::default_camera(), now);

    let result = session.handle_input(Input::Command(Command::Device(
        DeviceCommand::GetFirmwareDetails,
    )));
    assert!(matches!(result, Err(BcError::WrongRole)));
}

// ── Test: GetCapabilityDetails with channel ─────────────────────────

#[test]
fn test_device_get_ability_info() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    session
        .handle_input(Input::Command(Command::Device(
            DeviceCommand::GetCapabilityDetails { channel: 1 },
        )))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_CAPABILITY_DETAILS);

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<channelId>1</channelId>"));

    // Feed response
    let xml = b"<body>\
        <AbilityInfo version=\"1.1\">\
            <channelId>1</channelId>\
            <mainStream>1</mainStream>\
            <subStream>1</subStream>\
            <audio>0</audio>\
            <ptz>1</ptz>\
        </AbilityInfo>\
    </body>";
    let resp = make_wire_message(
        COMMAND_CAPABILITY_DETAILS,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &resp)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Device(DeviceEvent::CapabilityDetails(info))) => {
            assert_eq!(info.channel, 1);
            assert!(info.main_stream_supported);
            assert!(info.sub_stream_supported);
            assert!(!info.audio_supported);
            assert!(info.ptz_supported);
        }
        other => panic!("expected CapabilityDetails, got {other:?}"),
    }
}
