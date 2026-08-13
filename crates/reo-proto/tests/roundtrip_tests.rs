use arrayvec::ArrayString;
use reo_proto::{framing::ReadBuffer, header::PacketHeader, magic::*, xml::*};

#[test]
fn test_full_message_roundtrip_xor_encrypted() {
    use reo_proto::encryption::{decrypt_body_xor, encrypt_body_xor};

    // 1. Build an XML body
    let mut xml_buf = [0u8; 256];
    let xml_len = build_xml(&mut xml_buf, |b| {
        b.start_versioned("Preview", "1.1");
        b.u32_element("channelId", 0);
        b.text_element("streamType", "mainStream");
        b.end();
    })
    .unwrap();

    // 2. Copy XML body and encrypt it (XOR, offset 0)
    let mut body = xml_buf[..xml_len].to_vec();
    encrypt_body_xor(&mut body, 0);

    // 3. Create header
    let header = PacketHeader {
        msg_id: 3,
        body_len: body.len() as u32,
        encryption_offset: 0,
        status_class: 0x6414,
        extension: Some(0),
    };

    // 4. Serialize header + body into a wire buffer
    let mut wire = Vec::new();
    let mut hdr_buf = [0u8; HEADER_LEN_EXTENDED];
    let hdr_len = header.serialize(&mut hdr_buf);
    wire.extend_from_slice(&hdr_buf[..hdr_len]);
    wire.extend_from_slice(&body);

    // 5. Feed into ReadBuffer and parse
    let mut rb = ReadBuffer::new();
    rb.extend(&wire);
    let msg = rb.try_parse_message().unwrap().unwrap();

    assert_eq!(msg.header.msg_id, 3);
    assert!(msg.header.is_modern());
    assert!(msg.header.is_extended());
    assert!(msg.header.is_encrypted());

    // 6. Decrypt the body
    let mut decrypted = msg.body;
    decrypt_body_xor(&mut decrypted, 0);

    // 7. Parse the XML
    let mut stream_type = ArrayString::<32>::new();
    let mut channel_id = None;
    reo_proto::xml::parse_xml(&decrypted, |name, text| match name {
        "streamType" => {
            if let Ok(s) = ArrayString::try_from(text) {
                stream_type = s;
            }
        }
        "channelId" => channel_id = text.parse().ok(),
        _ => {}
    })
    .unwrap();

    assert_eq!(stream_type.as_str(), "mainStream");
    assert_eq!(channel_id, Some(0));
}

#[test]
fn test_full_message_roundtrip_aes_encrypted() {
    use reo_proto::encryption::{AesCipherState, decrypt_body_aes, encrypt_body_aes};

    let cipher = AesCipherState::from_credentials("testnonce", "admin");

    // Build XML
    let mut xml_buf = [0u8; 256];
    let xml_len = build_xml(&mut xml_buf, |b| {
        b.start_versioned("DeviceInfo", "1");
        b.text_element("model", "RLC-810A");
        b.end();
    })
    .unwrap();

    // Encrypt
    let mut body = xml_buf[..xml_len].to_vec();
    encrypt_body_aes(&cipher, &mut body, 0);

    // Header + wire
    let header = PacketHeader {
        msg_id: 146,
        body_len: body.len() as u32,
        encryption_offset: 0,
        status_class: 0x6414,
        extension: Some(0),
    };
    let mut wire = Vec::new();
    let mut hdr_buf = [0u8; HEADER_LEN_EXTENDED];
    let hdr_len = header.serialize(&mut hdr_buf);
    wire.extend_from_slice(&hdr_buf[..hdr_len]);
    wire.extend_from_slice(&body);

    // Parse from wire
    let mut rb = ReadBuffer::new();
    rb.extend(&wire);
    let msg = rb.try_parse_message().unwrap().unwrap();

    // Decrypt
    let mut decrypted = msg.body;
    decrypt_body_aes(&cipher, &mut decrypted, 0);

    // Extract model
    let mut model = ArrayString::<64>::new();
    let found = reo_proto::xml::extract_text(&decrypted, "model", &mut model).unwrap();
    assert!(found);
    assert_eq!(model.as_str(), "RLC-810A");
}

#[test]
fn test_full_message_roundtrip_unencrypted() {
    // Build XML
    let mut xml_buf = [0u8; 128];
    let xml_len = build_xml(&mut xml_buf, |b| {
        b.text_element("status", "ok");
    })
    .unwrap();

    let body = &xml_buf[..xml_len];

    // Header: encryption_offset == body_len means no encryption
    let header = PacketHeader {
        msg_id: 1,
        body_len: body.len() as u32,
        encryption_offset: body.len() as u32,
        status_class: make_status(BC_CLASS_LEGACY, 0),
        extension: None,
    };

    let mut wire = Vec::new();
    let mut hdr_buf = [0u8; HEADER_LEN_EXTENDED];
    let hdr_len = header.serialize(&mut hdr_buf);
    wire.extend_from_slice(&hdr_buf[..hdr_len]);
    wire.extend_from_slice(body);

    let mut rb = ReadBuffer::new();
    rb.extend(&wire);
    let msg = rb.try_parse_message().unwrap().unwrap();

    assert_eq!(msg.header.msg_id, 1);
    assert!(!msg.header.is_encrypted());

    // Body is plaintext XML, parse directly
    let mut status = ArrayString::<32>::new();
    let found = reo_proto::xml::extract_text(&msg.body, "status", &mut status).unwrap();
    assert!(found);
    assert_eq!(status.as_str(), "ok");
}
