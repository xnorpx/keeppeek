//! Phase 6 integration tests: Network configuration.
//!
//! Tests cover command → TcpSend wire format and response → Event round-trips
//! through the full BcSession state machine.

use arrayvec::ArrayString;
use reo_proto::{magic::*, network_cfg::*, *};
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

// ── Test: GetIp command ─────────────────────────────────────────────

#[test]
fn test_network_get_ip_command() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    session
        .handle_input(Input::Command(Command::Network(NetworkCommand::GetIp)))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_IP_READ);
    assert!(header.is_modern());

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<NetworkGeneral"));
}

// ── Test: IP response → Event::Network ──────────────────────────────

#[test]
fn test_network_ip_response() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let xml = b"<body>\
        <NetworkGeneral version=\"1.1\">\
            <dhcp>1</dhcp>\
            <ip>192.168.1.64</ip>\
            <mask>255.255.255.0</mask>\
            <gateway>192.168.1.1</gateway>\
            <dns1>8.8.8.8</dns1>\
            <dns2>8.8.4.4</dns2>\
        </NetworkGeneral>\
    </body>";

    let wire = make_wire_message(
        COMMAND_IP_READ,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Network(NetworkEvent::Ip(cfg))) => {
            assert!(cfg.dhcp);
            assert_eq!(cfg.ip.as_str(), "192.168.1.64");
            assert_eq!(cfg.mask.as_str(), "255.255.255.0");
            assert_eq!(cfg.gateway.as_str(), "192.168.1.1");
            assert_eq!(cfg.dns1.as_str(), "8.8.8.8");
            assert_eq!(cfg.dns2.as_str(), "8.8.4.4");
        }
        other => panic!("expected Network(Ip), got {other:?}"),
    }
}

// ── Test: SetIp command ─────────────────────────────────────────────

#[test]
fn test_network_set_ip_command() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let cfg = IpConfig {
        dhcp: false,
        ip: ArrayString::try_from("10.0.0.100").unwrap(),
        mask: ArrayString::try_from("255.255.255.0").unwrap(),
        gateway: ArrayString::try_from("10.0.0.1").unwrap(),
        dns1: ArrayString::try_from("1.1.1.1").unwrap(),
        dns2: ArrayString::try_from("1.0.0.1").unwrap(),
    };

    session
        .handle_input(Input::Command(Command::Network(NetworkCommand::SetIp(cfg))))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_IP_WRITE);

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<ip>10.0.0.100</ip>"));
    assert!(body_str.contains("<dhcp>0</dhcp>"));

    // Feed ack
    let ack = make_wire_message(
        COMMAND_IP_WRITE,
        b"<body><NetworkGeneral version=\"1.1\"></NetworkGeneral></body>",
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &ack)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Network(NetworkEvent::IpAck)) => {}
        other => panic!("expected IpAck, got {other:?}"),
    }
}

// ── Test: LinkType response ─────────────────────────────────────────

#[test]
fn test_network_link_type_response() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let xml = b"<body>\
        <LinkType version=\"1.1\">\
            <type>wifi</type>\
        </LinkType>\
    </body>";

    let wire = make_wire_message(
        COMMAND_LINK_TYPE,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Network(NetworkEvent::LinkType(info))) => {
            assert_eq!(info.link_type.as_str(), "wifi");
        }
        other => panic!("expected LinkType, got {other:?}"),
    }
}

// ── Test: WiFi signal response ──────────────────────────────────────

#[test]
fn test_network_wifi_signal_response() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let xml = b"<body>\
        <WifiSignal version=\"1.1\">\
            <signal>92</signal>\
        </WifiSignal>\
    </body>";

    let wire = make_wire_message(
        COMMAND_WIFI_SIGNAL,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Network(NetworkEvent::WifiSignal(info))) => {
            assert_eq!(info.signal, 92);
        }
        other => panic!("expected WifiSignal, got {other:?}"),
    }
}

// ── Test: CloudLoginKey response ────────────────────────────────────

#[test]
fn test_network_cloud_login_key_response() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let xml = b"<body>\
        <CloudLoginKey version=\"1.1\">\
            <key>secret_key_123</key>\
        </CloudLoginKey>\
    </body>";

    let wire = make_wire_message(
        COMMAND_CLOUD_LOGIN_KEY,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Network(NetworkEvent::CloudLoginKey(info))) => {
            assert_eq!(info.key.as_str(), "secret_key_123");
        }
        other => panic!("expected CloudLoginKey, got {other:?}"),
    }
}

// ── Test: CellularInfo response ─────────────────────────────────────

#[test]
fn test_network_cellular_info_response() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let xml = b"<body>\
        <CellularInfo version=\"1.1\">\
            <signal>60</signal>\
            <networkType>LTE</networkType>\
        </CellularInfo>\
    </body>";

    let wire = make_wire_message(
        COMMAND_CELLULAR_INFO,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Network(NetworkEvent::CellularInfo(info))) => {
            assert_eq!(info.signal, 60);
            assert_eq!(info.network_type.as_str(), "LTE");
        }
        other => panic!("expected CellularInfo, got {other:?}"),
    }
}

// ── Test: Network commands wrong role ───────────────────────────────

#[test]
fn test_network_commands_wrong_role() {
    let now = Instant::now();
    let mut session = BcSession::new(BcSessionConfig::default_camera(), now);

    let result = session.handle_input(Input::Command(Command::Network(NetworkCommand::GetIp)));
    assert!(matches!(result, Err(BcError::WrongRole)));
}

// ── Test: CloudBindInfo response ────────────────────────────────────

#[test]
fn test_network_cloud_bind_info_response() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let xml = b"<body>\
        <CloudBindInfo version=\"1.1\">\
            <bound>1</bound>\
        </CloudBindInfo>\
    </body>";

    let wire = make_wire_message(
        COMMAND_CLOUD_BIND_INFO,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Network(NetworkEvent::CloudBindInfo(info))) => {
            assert!(info.bound);
        }
        other => panic!("expected CloudBindInfo, got {other:?}"),
    }
}
