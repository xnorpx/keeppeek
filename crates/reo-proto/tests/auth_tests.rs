use arrayvec::ArrayString;
use reo_proto::{auth::*, magic::*, *};
use std::time::Instant;

// ── Helper ───────────────────────────────────────────────────────────

fn make_login_wire(msg_id: u32, body: &[u8], status_class: u32) -> Vec<u8> {
    let has_ext = (status_class >> 16) == BC_CLASS_MODERN_EXT as u32 || (status_class >> 16) == 0;
    let header = PacketHeader {
        msg_id,
        body_len: body.len() as u32,
        encryption_offset: body.len() as u32,
        status_class,
        extension: if has_ext { Some(0) } else { None },
    };
    let mut hdr_buf = [0u8; HEADER_LEN_EXTENDED];
    let hdr_len = header.serialize(&mut hdr_buf);
    let mut wire = Vec::new();
    wire.extend_from_slice(&hdr_buf[..hdr_len]);
    wire.extend_from_slice(body);
    wire
}

fn default_login_params() -> LoginParams {
    LoginParams {
        username: ArrayString::try_from("admin").unwrap(),
        password: ArrayString::try_from("password123").unwrap(),
        encryption: EncryptionMode::Aes,
    }
}

/// BCEncrypt a copy of the given plaintext (XOR with fixed key).
fn bc_encrypt(plaintext: &[u8]) -> Vec<u8> {
    let mut out = plaintext.to_vec();
    reo_proto::encryption::bc_xor(&mut out, 0);
    out
}

// ── Full login round-trip (4-step handshake) ─────────────────────────

#[test]
fn test_full_login_roundtrip() {
    let now = Instant::now();
    let mut session = BcSession::new(BcSessionConfig::default_client(), now);
    let params = default_login_params();

    // Step 1: Client sends login command → header-only LoginUpgrade
    session
        .handle_input(Input::Command(Command::Login(params)))
        .unwrap();
    assert_eq!(session.state(), SessionState::AwaitingNonce);

    let mut buf = [0u8; 4096];
    let _legacy_data = match session.poll_output(&mut buf).unwrap() {
        Output::TcpSend { data } => {
            let (h, _hdr_len) = PacketHeader::parse(data).unwrap();
            assert_eq!(h.msg_id, COMMAND_LOGIN);
            assert_eq!(h.body_len, 0); // header-only LoginUpgrade
            assert!(h.is_binary()); // LEGACY class
            data.to_vec()
        }
        other => panic!("expected TcpSend (login upgrade), got {other:?}"),
    };

    // Step 2: Camera responds with nonce (BCEncrypt'd body)
    let nonce_xml = br#"<body><Encryption version="2"><type>aes</type><nonce>ROUNDTRIP_NONCE</nonce></Encryption></body>"#;
    let enc_nonce = bc_encrypt(nonce_xml);
    let nonce_wire = make_login_wire(COMMAND_LOGIN, &enc_nonce, make_status(0, 0xDD02));
    session
        .handle_input(Input::TcpData(now, &nonce_wire))
        .unwrap();
    assert_eq!(session.state(), SessionState::AwaitingLoginConfirm);

    // Step 3: Session automatically sends modern login (BCEncrypt'd body)
    let _modern_data = match session.poll_output(&mut buf).unwrap() {
        Output::TcpSend { data } => {
            let (h, hdr_len) = PacketHeader::parse(data).unwrap();
            assert_eq!(h.msg_id, COMMAND_LOGIN);
            assert!(h.is_modern());
            assert!(h.is_extended());
            // Decrypt the BCEncrypt'd body to verify XML contents
            let mut body = data[hdr_len..].to_vec();
            reo_proto::encryption::bc_xor(&mut body, 0);
            let xml = core::str::from_utf8(&body).unwrap();
            assert!(xml.contains("<LoginUser"));
            assert!(xml.contains("<LoginNet"));
            // Hashed, not plaintext
            assert!(!xml.contains("password123"));
            data.to_vec()
        }
        other => panic!("expected TcpSend (modern login), got {other:?}"),
    };

    // Step 4: Camera confirms login (BCEncrypt'd body)
    let confirm_xml = br#"<body><LoginUser version="2"><userName>admin</userName><result>ok</result><userId>42</userId></LoginUser><DeviceInfo version="2"><model>RLC-810A</model><serialNumber>SN99</serialNumber><firmVer>v3.1.2</firmVer><channelNum>4</channelNum></DeviceInfo></body>"#;
    let enc_confirm = bc_encrypt(confirm_xml);
    let confirm_wire = make_login_wire(COMMAND_LOGIN, &enc_confirm, make_status(0, 0));
    session
        .handle_input(Input::TcpData(now, &confirm_wire))
        .unwrap();
    assert_eq!(session.state(), SessionState::Connected);

    // Poll the LoggedIn event
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::LoggedIn(result)) => {
            assert_eq!(result.user_id, 42);
            assert_eq!(result.camera_identity.model.as_str(), "RLC-810A");
            assert_eq!(result.camera_identity.serial.as_str(), "SN99");
            assert_eq!(result.camera_identity.firmware.as_str(), "v3.1.2");
            assert_eq!(result.camera_identity.channel_count, 4);
            assert_eq!(result.encryption, EncryptionMode::Aes);
        }
        other => panic!("expected LoggedIn event, got {other:?}"),
    }

    // After login, session should be functional (e.g. keepalive)
    match session.poll_output(&mut buf).unwrap() {
        Output::Timeout(_) => {} // normal idle state
        other => panic!("expected Timeout, got {other:?}"),
    }
}

// ── Encryption class values on the wire ──────────────────────────────

#[test]
fn test_encryption_class_on_wire() {
    // The modern login (Step 3) sends response_code=0
    // regardless of encryption mode. The encryption mode was already negotiated
    // in Steps 1-2 via the legacy login header.
    for mode in [
        EncryptionMode::None,
        EncryptionMode::BcEncrypt,
        EncryptionMode::Aes,
        EncryptionMode::FullAes,
    ] {
        let now = Instant::now();
        let mut session = BcSession::new(BcSessionConfig::default_client(), now);
        let params = LoginParams {
            username: ArrayString::try_from("u").unwrap(),
            password: ArrayString::try_from("p").unwrap(),
            encryption: mode,
        };

        session
            .handle_input(Input::Command(Command::Login(params)))
            .unwrap();

        let mut buf = [0u8; 4096];
        // Drain login upgrade
        session.poll_output(&mut buf).unwrap();

        // Feed nonce (camera supports up to FullAes, BCEncrypt'd body)
        let nonce_xml = br#"<body><Encryption version="2"><type>aes</type><nonce>N</nonce></Encryption></body>"#;
        let enc_nonce = bc_encrypt(nonce_xml);
        let wire = make_login_wire(COMMAND_LOGIN, &enc_nonce, make_status(0, 0xDD12));
        session.handle_input(Input::TcpData(now, &wire)).unwrap();

        // Modern login (Step 3) sends response_code = 0
        match session.poll_output(&mut buf).unwrap() {
            Output::TcpSend { data } => {
                let (h, _) = PacketHeader::parse(data).unwrap();
                assert_eq!(
                    h.response_code(),
                    0,
                    "mode {:?} should produce response_code 0, got {:#06x}",
                    mode,
                    h.response_code()
                );
                assert_eq!(h.bc_class(), BC_CLASS_MODERN_EXT);
            }
            other => panic!("expected TcpSend for {mode:?}, got {other:?}"),
        }
    }
}

// ── Login failure ────────────────────────────────────────────────────

#[test]
fn test_login_failure_event() {
    let now = Instant::now();
    let mut session = BcSession::new(BcSessionConfig::default_client(), now);
    let params = default_login_params();

    session
        .handle_input(Input::Command(Command::Login(params)))
        .unwrap();

    let mut buf = [0u8; 4096];
    session.poll_output(&mut buf).unwrap(); // drain login upgrade

    // Nonce (BCEncrypt'd)
    let nonce_xml =
        br#"<body><Encryption version="2"><type>aes</type><nonce>N</nonce></Encryption></body>"#;
    let enc_nonce = bc_encrypt(nonce_xml);
    let wire = make_login_wire(COMMAND_LOGIN, &enc_nonce, make_status(0, 0xDD02));
    session.handle_input(Input::TcpData(now, &wire)).unwrap();
    while let Ok(Output::TcpSend { .. }) = session.poll_output(&mut buf) {}

    // Camera rejects: no userId (BCEncrypt'd)
    let reject = br#"<body><LoginUser><result>error</result></LoginUser></body>"#;
    let enc_reject = bc_encrypt(reject);
    let reject_wire = make_login_wire(COMMAND_LOGIN, &enc_reject, make_status(0, 0));
    session
        .handle_input(Input::TcpData(now, &reject_wire))
        .unwrap();

    assert_eq!(session.state(), SessionState::Disconnected);
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::LoginFailed(_)) => {}
        other => panic!("expected LoginFailed, got {other:?}"),
    }
}

// ── Encryption downgrade negotiation ─────────────────────────────────

#[test]
fn test_encryption_downgrade_negotiation() {
    let now = Instant::now();
    let mut session = BcSession::new(BcSessionConfig::default_client(), now);
    let params = LoginParams {
        username: ArrayString::try_from("admin").unwrap(),
        password: ArrayString::try_from("pass").unwrap(),
        encryption: EncryptionMode::FullAes,
    };

    session
        .handle_input(Input::Command(Command::Login(params)))
        .unwrap();

    let mut buf = [0u8; 4096];
    session.poll_output(&mut buf).unwrap(); // drain login upgrade

    // Camera only supports BCEncrypt (BCEncrypt'd nonce)
    let nonce_xml =
        br#"<body><Encryption version="1"><type>bc</type><nonce>ABC</nonce></Encryption></body>"#;
    let enc_nonce = bc_encrypt(nonce_xml);
    let wire = make_login_wire(COMMAND_LOGIN, &enc_nonce, make_status(0, 0xDD01));
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    // Modern login should use BcEncrypt (downgraded from FullAes)
    match session.poll_output(&mut buf).unwrap() {
        Output::TcpSend { data } => {
            let (h, _) = PacketHeader::parse(data).unwrap();
            assert_eq!(h.response_code(), 0);
            assert_eq!(h.bc_class(), BC_CLASS_MODERN_EXT);
        }
        other => panic!("expected TcpSend, got {other:?}"),
    }

    // Confirm login with downgraded encryption (BCEncrypt'd)
    let confirm_xml = br#"<body><LoginUser><userId>1</userId></LoginUser></body>"#;
    let enc_confirm = bc_encrypt(confirm_xml);
    let confirm_wire = make_login_wire(COMMAND_LOGIN, &enc_confirm, make_status(0, 0));
    session
        .handle_input(Input::TcpData(now, &confirm_wire))
        .unwrap();

    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::LoggedIn(result)) => {
            assert_eq!(result.encryption, EncryptionMode::BcEncrypt);
        }
        other => panic!("expected LoggedIn with BcEncrypt, got {other:?}"),
    }
}

// ── Logout ───────────────────────────────────────────────────────────

#[test]
fn test_logout_integration() {
    let now = Instant::now();
    let mut session = BcSession::new(BcSessionConfig::default_client(), now);

    // Quick login
    let params = default_login_params();
    session
        .handle_input(Input::Command(Command::Login(params)))
        .unwrap();

    // Rush through login handshake
    let mut buf = [0u8; 4096];
    session.poll_output(&mut buf).unwrap(); // login upgrade

    let nonce =
        br#"<body><Encryption version="2"><type>aes</type><nonce>X</nonce></Encryption></body>"#;
    let enc_nonce = bc_encrypt(nonce);
    session
        .handle_input(Input::TcpData(
            now,
            &make_login_wire(COMMAND_LOGIN, &enc_nonce, make_status(0, 0xDD02)),
        ))
        .unwrap();
    while let Ok(Output::TcpSend { .. }) = session.poll_output(&mut buf) {}

    let confirm = br#"<body><LoginUser><userId>1</userId></LoginUser></body>"#;
    let enc_confirm = bc_encrypt(confirm);
    session
        .handle_input(Input::TcpData(
            now,
            &make_login_wire(COMMAND_LOGIN, &enc_confirm, make_status(0, 0)),
        ))
        .unwrap();
    // Drain LoggedIn event
    session.poll_output(&mut buf).unwrap();

    assert_eq!(session.state(), SessionState::Connected);

    // Now logout
    session
        .handle_input(Input::Command(Command::Logout))
        .unwrap();
    assert_eq!(session.state(), SessionState::Disconnected);

    match session.poll_output(&mut buf).unwrap() {
        Output::TcpSend { data } => {
            let (h, _) = PacketHeader::parse(data).unwrap();
            assert_eq!(h.msg_id, COMMAND_LOGOUT);
            assert_eq!(h.body_len, 0);
        }
        other => panic!("expected TcpSend (logout), got {other:?}"),
    }
}
