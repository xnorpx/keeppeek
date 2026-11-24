use super::*;
use reo_proto::{
    COMMAND_LOGIN, COMMAND_TALK_CAPABILITIES, CameraIdentity, PacketHeader, auth, encryption,
    magic::{
        BC_CLASS_LEGACY, BC_CLASS_MODERN_EXT, HEADER_LEN_EXTENDED, HEADER_LEN_SHORT, make_status,
    },
};
use std::net::TcpListener;

const CHANNEL: u8 = 3;

fn take_packet(socket: &mut TcpStream) -> (PacketHeader, Vec<u8>) {
    let mut wire = [0; HEADER_LEN_EXTENDED];
    socket.read_exact(&mut wire[..HEADER_LEN_SHORT]).unwrap();
    let (_, len) = PacketHeader::parse(&wire).unwrap();
    socket.read_exact(&mut wire[HEADER_LEN_SHORT..len]).unwrap();
    let (header, _) = PacketHeader::parse(&wire).unwrap();
    assert!(header.body_len < BUFFER_BYTES as u32);
    let mut body = vec![0; header.body_len as usize];
    socket.read_exact(&mut body).unwrap();
    (header, body)
}

fn send_packet(socket: &mut TcpStream, header: PacketHeader, xml: &mut [u8]) {
    let (extension, payload) = xml.split_at_mut(header.extension.unwrap_or(0) as usize);
    encryption::bc_xor(extension, header.channel_id());
    encryption::bc_xor(payload, header.channel_id());
    let mut wire = [0; HEADER_LEN_EXTENDED];
    let size = header.serialize(&mut wire);
    socket.write_all(&wire[..size]).unwrap();
    socket.write_all(xml).unwrap();
}

fn authenticate(socket: &mut TcpStream) {
    let (request, body) = take_packet(socket);
    assert_eq!(request.msg_id, COMMAND_LOGIN);
    assert!(request.is_binary() && body.is_empty());
    let mut xml = [0; reo_proto::MAX_XML_BODY];
    let len =
        auth::build_nonce_response("test-nonce", EncryptionMode::BcEncrypt, &mut xml).unwrap();
    send_packet(
        socket,
        PacketHeader {
            body_len: len as u32,
            encryption_offset: 0,
            status_class: make_status(BC_CLASS_LEGACY, 0xdd01),
            extension: None,
            ..request
        },
        &mut xml[..len],
    );
    let (request, mut body) = take_packet(socket);
    assert_eq!(request.msg_id, COMMAND_LOGIN);
    assert!(request.is_modern());
    encryption::bc_xor(&mut body, 0);
    let (user, password) = auth::parse_modern_login(&body).unwrap();
    assert!(auth::validate_credentials(
        "test-nonce",
        "viewer",
        "fixture",
        &user,
        &password
    ));
    let len = auth::build_login_confirmation(1, &CameraIdentity::default(), &mut xml).unwrap();
    send_packet(
        socket,
        PacketHeader {
            body_len: len as u32,
            status_class: make_status(BC_CLASS_MODERN_EXT, 0),
            ..request
        },
        &mut xml[..len],
    );
}

#[test]
fn discovery_rejects_unrepresentable_credentials_before_connecting() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let username = "user".repeat(reo_proto::USERNAME_CAP);
    let password = "🙂".repeat(reo_proto::PASSWORD_CAP);
    assert!(!compatible(address, &username, "fixture", CHANNEL).unwrap());
    assert!(!compatible(address, "viewer", &password, CHANNEL).unwrap());
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn standalone_discovery_authenticates_and_never_opens_a_speaker() {
    for (reply, expected) in [
        (Some("adpcm"), true),
        (Some("unknown"), false),
        (None, false),
    ] {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let camera = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(DEADLINE)).unwrap();
            authenticate(&mut socket);
            let (request, mut extension) = take_packet(&mut socket);
            assert_eq!(request.msg_id, COMMAND_TALK_CAPABILITIES);
            assert_eq!(request.channel_id(), CHANNEL);
            assert_ne!(request.message_number(), 0);
            assert_eq!(request.extension, Some(request.body_len));
            encryption::bc_xor(&mut extension, CHANNEL);
            assert!(
                std::str::from_utf8(&extension)
                    .unwrap()
                    .contains("<channelId>3</channelId>")
            );
            if let Some(codec) = reply {
                let mut xml = extension;
                xml.extend_from_slice(format!(r#"<body><TalkAbility version="1.1"><duplexList><duplex>fullDuplex</duplex></duplexList><audioStreamModeList><audioStreamMode>speaker</audioStreamMode></audioStreamModeList><audioConfigList><audioConfig><audioType>{codec}</audioType><sampleRate>16000</sampleRate><samplePrecision>16</samplePrecision><lengthPerEncoder>640</lengthPerEncoder><soundTrack>mono</soundTrack></audioConfig></audioConfigList></TalkAbility></body>"#).as_bytes());
                send_packet(
                    &mut socket,
                    PacketHeader {
                        body_len: xml.len() as u32,
                        status_class: make_status(BC_CLASS_MODERN_EXT, 200),
                        ..request
                    },
                    &mut xml,
                );
                assert_eq!(socket.read(&mut [0; 1]).unwrap(), 0);
            }
        });
        let result = compatible(address, "viewer", "fixture", CHANNEL);
        camera.join().unwrap();
        assert_eq!(result.unwrap(), expected);
    }
}
