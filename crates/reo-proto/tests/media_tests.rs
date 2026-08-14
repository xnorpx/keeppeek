use reo_proto::{framing::ReadBuffer, header::PacketHeader, magic::*, media::*};

/// Build a Stream message (binary payload) wrapping media frames.
fn make_stream_message(media_payload: &[u8]) -> Vec<u8> {
    let header = PacketHeader {
        msg_id: reo_proto::COMMAND_STREAM,
        body_len: media_payload.len() as u32,
        encryption_offset: media_payload.len() as u32, // unencrypted
        status_class: make_status(BC_CLASS_LEGACY, 0), // binary payload
        extension: None,
    };
    let mut wire = Vec::new();
    let mut hdr_buf = [0u8; HEADER_LEN_EXTENDED];
    let hdr_len = header.serialize(&mut hdr_buf);
    wire.extend_from_slice(&hdr_buf[..hdr_len]);
    wire.extend_from_slice(media_payload);
    wire
}

fn make_test_video_frame(magic: u32, codec: &[u8; 4], data: &[u8], microseconds: u32) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&magic.to_le_bytes());
    buf.extend_from_slice(codec);
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

fn make_test_stream_metadata(magic: u32, width: u32, height: u32, fps: u8) -> Vec<u8> {
    let header_size: u32 = 30;
    let mut buf = Vec::new();
    buf.extend_from_slice(&magic.to_le_bytes());
    buf.extend_from_slice(&header_size.to_le_bytes());
    buf.extend_from_slice(&width.to_le_bytes());
    buf.extend_from_slice(&height.to_le_bytes());
    buf.push(0); // reserved
    buf.push(fps);
    buf.extend_from_slice(&[25, 1, 1, 0, 0, 0]); // start time
    buf.extend_from_slice(&[25, 1, 1, 1, 0, 0]); // end time
    while buf.len() % 8 != 0 {
        buf.push(0);
    }
    buf
}

fn make_test_aac_frame(data: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&MEDIA_MAGIC_AAC.to_le_bytes());
    buf.extend_from_slice(&(data.len() as u16).to_le_bytes());
    buf.extend_from_slice(&(data.len() as u16).to_le_bytes());
    buf.extend_from_slice(data);
    while buf.len() % 8 != 0 {
        buf.push(0);
    }
    buf
}

#[test]
fn test_stream_message_to_media_frames() {
    // Build a media payload with info + I-frame + AAC audio
    let mut media_payload = Vec::new();
    media_payload.extend(make_test_stream_metadata(
        MEDIA_MAGIC_INFO_V1,
        1920,
        1080,
        30,
    ));
    let video_data = vec![0x00, 0x00, 0x00, 0x01, 0x65, 0xAA, 0xBB, 0xCC];
    media_payload.extend(make_test_video_frame(
        MEDIA_MAGIC_IFRAME_BASE,
        b"H264",
        &video_data,
        16667,
    ));
    let audio_data = vec![0xFF, 0xF1, 0x50, 0x80, 0x02, 0x00, 0x00, 0x00];
    media_payload.extend(make_test_aac_frame(&audio_data));

    // Wrap in a Baichuan Stream message
    let wire = make_stream_message(&media_payload);

    // Parse from wire
    let mut rb = ReadBuffer::new();
    rb.extend(&wire);
    let msg = rb.try_parse_message().unwrap().unwrap();
    assert_eq!(msg.header.msg_id, reo_proto::COMMAND_STREAM);
    assert!(msg.header.is_binary());

    // Parse media frames from the body
    let frames: Vec<_> = parse_media_frames(&msg.body)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    assert_eq!(frames.len(), 3);
    match &frames[0] {
        MediaFrame::Info(info) => {
            assert_eq!(info.width, 1920);
            assert_eq!(info.height, 1080);
            assert_eq!(info.fps, 30);
        }
        other => panic!("expected Info, got {other:?}"),
    }
    match &frames[1] {
        MediaFrame::Video(v) => {
            assert!(v.is_keyframe);
            assert_eq!(v.codec, VideoCodec::H264);
            assert_eq!(v.data, &video_data);
        }
        other => panic!("expected Video, got {other:?}"),
    }
    match &frames[2] {
        MediaFrame::Audio(a) => {
            assert_eq!(a.codec, AudioCodec::Aac);
            assert_eq!(a.data, &audio_data);
        }
        other => panic!("expected Audio, got {other:?}"),
    }
}

#[test]
fn test_xor_encrypted_stream_with_media_frames() {
    use reo_proto::encryption::{decrypt_body_xor, encrypt_body_xor};

    // Build media payload
    let video_data = vec![0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE, 0xBA, 0xBE];
    let media_payload = make_test_video_frame(
        MEDIA_MAGIC_PFRAME_BASE + 1, // channel 1 P-frame
        b"H265",
        &video_data,
        42000,
    );

    // Encrypt the payload
    let mut encrypted = media_payload.clone();
    encrypt_body_xor(&mut encrypted, 0);

    // Build wire message
    let header = PacketHeader {
        msg_id: reo_proto::COMMAND_STREAM,
        body_len: encrypted.len() as u32,
        encryption_offset: 0,
        status_class: make_status(BC_CLASS_LEGACY, 0),
        extension: None,
    };
    let mut wire = Vec::new();
    let mut hdr_buf = [0u8; HEADER_LEN_EXTENDED];
    let hdr_len = header.serialize(&mut hdr_buf);
    wire.extend_from_slice(&hdr_buf[..hdr_len]);
    wire.extend_from_slice(&encrypted);

    // Parse from wire
    let mut rb = ReadBuffer::new();
    rb.extend(&wire);
    let msg = rb.try_parse_message().unwrap().unwrap();
    assert!(msg.header.is_binary());

    // Decrypt
    let mut body = msg.body;
    decrypt_body_xor(&mut body, 0);
    assert_eq!(body, media_payload);

    // Parse media frames from decrypted body
    let frames: Vec<_> = parse_media_frames(&body)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(frames.len(), 1);
    match &frames[0] {
        MediaFrame::Video(v) => {
            assert!(!v.is_keyframe);
            assert_eq!(v.channel, 1);
            assert_eq!(v.codec, VideoCodec::H265);
            assert_eq!(v.data, &video_data);
            assert_eq!(v.microseconds, 42000);
        }
        other => panic!("expected Video, got {other:?}"),
    }
}

#[test]
fn test_media_magic_constants_are_distinct() {
    let magics: Vec<u32> = (0..=9u8)
        .map(|ch| MEDIA_MAGIC_IFRAME_BASE + ch as u32)
        .chain((0..=9u8).map(|ch| MEDIA_MAGIC_PFRAME_BASE + ch as u32))
        .chain([
            MEDIA_MAGIC_INFO_V1,
            MEDIA_MAGIC_INFO_V2,
            MEDIA_MAGIC_AAC,
            MEDIA_MAGIC_ADPCM,
        ])
        .collect();

    for i in 0..magics.len() {
        for j in (i + 1)..magics.len() {
            assert_ne!(
                magics[i], magics[j],
                "duplicate media magic {:#010x} at indices {i} and {j}",
                magics[i]
            );
        }
    }
}
