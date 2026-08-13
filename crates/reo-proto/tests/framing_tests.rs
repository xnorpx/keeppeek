mod common;

use common::make_header_bytes;
use reo_proto::{framing::ReadBuffer, magic::*};

#[test]
fn test_framing_single_complete_message() {
    let mut rb = ReadBuffer::new();
    let body = b"hello world";
    let header_bytes = make_header_bytes(
        80,
        body.len() as u32,
        body.len() as u32,
        make_status(BC_CLASS_LEGACY, 0),
        None,
    );
    let mut data = header_bytes;
    data.extend_from_slice(body);

    rb.extend(&data);
    let msg = rb.try_parse_message().unwrap().unwrap();
    assert_eq!(msg.header.msg_id, 80);
    assert_eq!(msg.body, body);
    assert!(rb.is_empty());
}

#[test]
fn test_framing_partial_header() {
    let mut rb = ReadBuffer::new();
    let header_bytes = make_header_bytes(1, 4, 4, make_status(BC_CLASS_LEGACY, 0), None);
    let body = [0xAA, 0xBB, 0xCC, 0xDD];

    // Feed only first 10 bytes of header
    rb.extend(&header_bytes[..10]);
    assert!(rb.try_parse_message().unwrap().is_none());

    // Feed rest of header + body
    rb.extend(&header_bytes[10..]);
    rb.extend(&body);
    let msg = rb.try_parse_message().unwrap().unwrap();
    assert_eq!(msg.header.msg_id, 1);
    assert_eq!(msg.body, body);
}

#[test]
fn test_framing_partial_body() {
    let mut rb = ReadBuffer::new();
    let body = [0x42u8; 100];
    let header_bytes = make_header_bytes(56, 100, 100, make_status(BC_CLASS_LEGACY, 0), None);

    // Feed full header + half body
    rb.extend(&header_bytes);
    rb.extend(&body[..50]);
    assert!(rb.try_parse_message().unwrap().is_none());

    // Feed remaining body
    rb.extend(&body[50..]);
    let msg = rb.try_parse_message().unwrap().unwrap();
    assert_eq!(msg.header.msg_id, 56);
    assert_eq!(msg.body.len(), 100);
}

#[test]
fn test_framing_two_messages_in_one_read() {
    let mut rb = ReadBuffer::new();

    let body1 = b"first";
    let h1 = make_header_bytes(
        1,
        body1.len() as u32,
        body1.len() as u32,
        make_status(BC_CLASS_LEGACY, 0),
        None,
    );

    let body2 = b"second";
    let h2 = make_header_bytes(
        2,
        body2.len() as u32,
        body2.len() as u32,
        make_status(BC_CLASS_LEGACY, 0),
        None,
    );

    let mut data = h1;
    data.extend_from_slice(body1);
    data.extend(h2);
    data.extend_from_slice(body2);

    rb.extend(&data);

    let msg1 = rb.try_parse_message().unwrap().unwrap();
    assert_eq!(msg1.header.msg_id, 1);
    assert_eq!(msg1.body, b"first");

    let msg2 = rb.try_parse_message().unwrap().unwrap();
    assert_eq!(msg2.header.msg_id, 2);
    assert_eq!(msg2.body, b"second");

    assert!(rb.try_parse_message().unwrap().is_none());
}

#[test]
fn test_framing_byte_at_a_time() {
    let body = b"payload";
    let header_bytes = make_header_bytes(
        23,
        body.len() as u32,
        body.len() as u32,
        make_status(BC_CLASS_LEGACY, 0),
        None,
    );
    let mut full = header_bytes;
    full.extend_from_slice(body);

    let mut rb = ReadBuffer::new();
    for (i, &byte) in full.iter().enumerate() {
        rb.extend(&[byte]);
        if i < full.len() - 1 {
            assert!(rb.try_parse_message().unwrap().is_none());
        }
    }
    let msg = rb.try_parse_message().unwrap().unwrap();
    assert_eq!(msg.header.msg_id, 23);
    assert_eq!(msg.body, b"payload");
}

#[test]
fn test_framing_zero_length_body() {
    let mut rb = ReadBuffer::new();
    let header_bytes = make_header_bytes(2, 0, 0, make_status(BC_CLASS_LEGACY, 0), None);
    rb.extend(&header_bytes);
    let msg = rb.try_parse_message().unwrap().unwrap();
    assert_eq!(msg.header.msg_id, 2);
    assert!(msg.body.is_empty());
}

#[test]
fn test_framing_large_message() {
    let mut rb = ReadBuffer::new();
    let body = vec![0xABu8; 1_000_000];
    let header_bytes = make_header_bytes(
        3,
        body.len() as u32,
        body.len() as u32,
        make_status(BC_CLASS_LEGACY, 0),
        None,
    );

    rb.extend(&header_bytes);
    rb.extend(&body);
    let msg = rb.try_parse_message().unwrap().unwrap();
    assert_eq!(msg.header.msg_id, 3);
    assert_eq!(msg.body.len(), 1_000_000);
    assert!(msg.header.is_binary());
}

#[test]
fn test_framing_skips_garbage_before_magic() {
    let mut rb = ReadBuffer::new();
    let garbage = [0xFF, 0xFE, 0xFD, 0xFC, 0xFB];
    let body = b"data";
    let header_bytes = make_header_bytes(
        80,
        body.len() as u32,
        body.len() as u32,
        make_status(BC_CLASS_LEGACY, 0),
        None,
    );

    rb.extend(&garbage);
    rb.extend(&header_bytes);
    rb.extend(body);

    let msg = rb.try_parse_message().unwrap().unwrap();
    assert_eq!(msg.header.msg_id, 80);
    assert_eq!(msg.body, b"data");
}

#[test]
fn test_framing_extended_header_message() {
    let mut rb = ReadBuffer::new();
    let body = b"extended_body";
    let header_bytes = make_header_bytes(
        146,
        body.len() as u32,
        body.len() as u32,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(99),
    );

    rb.extend(&header_bytes);
    rb.extend(body);

    let msg = rb.try_parse_message().unwrap().unwrap();
    assert_eq!(msg.header.msg_id, 146);
    assert_eq!(msg.header.extension, Some(99));
    assert!(msg.header.is_extended());
    assert!(msg.header.is_modern());
    assert_eq!(msg.body, b"extended_body");
}

#[test]
fn test_framing_garbage_between_two_messages() {
    let mut rb = ReadBuffer::new();

    let body1 = b"msg1";
    let h1 = make_header_bytes(
        10,
        body1.len() as u32,
        body1.len() as u32,
        make_status(BC_CLASS_LEGACY, 0),
        None,
    );

    let garbage = [0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x11, 0x22];

    let body2 = b"msg2";
    let h2 = make_header_bytes(
        20,
        body2.len() as u32,
        body2.len() as u32,
        make_status(BC_CLASS_LEGACY, 0),
        None,
    );

    rb.extend(&h1);
    rb.extend(body1);
    rb.extend(&garbage);
    rb.extend(&h2);
    rb.extend(body2);

    let msg1 = rb.try_parse_message().unwrap().unwrap();
    assert_eq!(msg1.header.msg_id, 10);
    assert_eq!(msg1.body, b"msg1");

    // Second parse should skip garbage and find msg2
    let msg2 = rb.try_parse_message().unwrap().unwrap();
    assert_eq!(msg2.header.msg_id, 20);
    assert_eq!(msg2.body, b"msg2");

    assert!(rb.try_parse_message().unwrap().is_none());
}

#[test]
fn test_framing_only_garbage() {
    let mut rb = ReadBuffer::new();
    // Need >= 20 bytes so framing attempts header parse and triggers garbage scan
    let garbage = [0xFF; 24];
    rb.extend(&garbage);
    assert!(rb.try_parse_message().unwrap().is_none());
    // Buffer should have retained at most 3 bytes (partial magic scan)
    assert!(rb.len() <= 3);
}

#[test]
fn test_framing_interleaved_short_and_extended() {
    let mut rb = ReadBuffer::new();

    // Short header message
    let body1 = b"short";
    let h1 = make_header_bytes(
        1,
        body1.len() as u32,
        body1.len() as u32,
        make_status(BC_CLASS_LEGACY, 0),
        None,
    );

    // Extended header message
    let body2 = b"extended";
    let h2 = make_header_bytes(
        2,
        body2.len() as u32,
        body2.len() as u32,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(42),
    );

    // Another short one
    let body3 = b"short2";
    let h3 = make_header_bytes(
        3,
        body3.len() as u32,
        body3.len() as u32,
        make_status(BC_CLASS_LEGACY, 0),
        None,
    );

    rb.extend(&h1);
    rb.extend(body1);
    rb.extend(&h2);
    rb.extend(body2);
    rb.extend(&h3);
    rb.extend(body3);

    let msg1 = rb.try_parse_message().unwrap().unwrap();
    assert_eq!(msg1.header.msg_id, 1);
    assert_eq!(msg1.header.header_len(), 20);

    let msg2 = rb.try_parse_message().unwrap().unwrap();
    assert_eq!(msg2.header.msg_id, 2);
    assert_eq!(msg2.header.header_len(), 24);
    assert_eq!(msg2.header.extension, Some(42));

    let msg3 = rb.try_parse_message().unwrap().unwrap();
    assert_eq!(msg3.header.msg_id, 3);
    assert_eq!(msg3.header.header_len(), 20);

    assert!(rb.try_parse_message().unwrap().is_none());
}
