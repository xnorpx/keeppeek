mod common;

use common::make_header_bytes;
use reo_proto::{BcError, header::PacketHeader, magic::*};

#[test]
fn test_header_parse_20_bytes() {
    let data = make_header_bytes(1, 64, 64, make_status(BC_CLASS_LEGACY, 0), None);
    let (header, consumed) = PacketHeader::parse(&data).unwrap();
    assert_eq!(consumed, 20);
    assert_eq!(header.msg_id, 1);
    assert_eq!(header.body_len, 64);
    assert_eq!(header.encryption_offset, 64);
    assert_eq!(header.status_class, make_status(BC_CLASS_LEGACY, 0));
    assert_eq!(header.extension, None);
}

#[test]
fn test_header_parse_24_bytes() {
    let data = make_header_bytes(3, 1024, 0, 0x6414, Some(42));
    let (header, consumed) = PacketHeader::parse(&data).unwrap();
    assert_eq!(consumed, 24);
    assert_eq!(header.msg_id, 3);
    assert_eq!(header.body_len, 1024);
    assert_eq!(header.encryption_offset, 0);
    assert_eq!(header.status_class, 0x6414);
    assert_eq!(header.extension, Some(42));
}

#[test]
fn test_header_bad_magic() {
    let mut data = make_header_bytes(1, 0, 0, 0, None);
    data[0] = 0xFF;
    data[1] = 0xFF;
    data[2] = 0xFF;
    data[3] = 0xFF;
    match PacketHeader::parse(&data) {
        Err(BcError::BadMagic([0xFF, 0xFF, 0xFF, 0xFF])) => {}
        other => panic!("expected BadMagic, got {other:?}"),
    }
}

#[test]
fn test_header_incomplete() {
    let data = [0xf0, 0xde, 0xbc, 0x0a, 0x01, 0x00]; // only 6 bytes
    match PacketHeader::parse(&data) {
        Err(BcError::Incomplete) => {}
        other => panic!("expected Incomplete, got {other:?}"),
    }
}

#[test]
fn test_header_incomplete_extended() {
    // 20 bytes with make_status(BC_CLASS_MODERN_EXT, 0) set but no extension bytes
    let data = make_header_bytes(1, 0, 0, make_status(BC_CLASS_MODERN_EXT, 0), None);
    // This produces 20 bytes, but parse expects 24 for extended
    match PacketHeader::parse(&data) {
        Err(BcError::Incomplete) => {}
        other => {
            panic!("expected Incomplete for extended header with only 20 bytes, got {other:?}")
        }
    }
}

#[test]
fn test_header_roundtrip() {
    let original = PacketHeader {
        msg_id: 272,
        body_len: 512,
        encryption_offset: 100,
        status_class: 0x6414,
        extension: Some(7),
    };
    let mut buf = [0u8; HEADER_LEN_EXTENDED];
    let len = original.serialize(&mut buf);
    let (parsed, consumed) = PacketHeader::parse(&buf[..len]).unwrap();
    assert_eq!(consumed, 24);
    assert_eq!(parsed, original);
}

#[test]
fn test_header_roundtrip_20() {
    let original = PacketHeader {
        msg_id: 2,
        body_len: 0,
        encryption_offset: 0,
        status_class: make_status(BC_CLASS_LEGACY, 0),
        extension: None,
    };
    let mut buf = [0u8; HEADER_LEN_EXTENDED];
    let len = original.serialize(&mut buf);
    let (parsed, consumed) = PacketHeader::parse(&buf[..len]).unwrap();
    assert_eq!(consumed, 20);
    assert_eq!(parsed, original);
}

#[test]
fn test_header_is_copy() {
    let h = PacketHeader {
        msg_id: 1,
        body_len: 0,
        encryption_offset: 0,
        status_class: 0,
        extension: None,
    };
    let h2 = h; // Copy
    let h3 = h; // Copy again - if not Copy, this would error
    assert_eq!(h2, h3);
}

#[test]
fn test_header_is_binary() {
    let h = PacketHeader {
        msg_id: 3,
        body_len: 0,
        encryption_offset: 0,
        status_class: make_status(BC_CLASS_LEGACY, 0),
        extension: None,
    };
    assert!(h.is_binary());

    let h2 = PacketHeader {
        msg_id: 1,
        body_len: 0,
        encryption_offset: 0,
        status_class: 0,
        extension: None,
    };
    assert!(!h2.is_binary());
}

#[test]
fn test_header_is_modern() {
    let h = PacketHeader {
        msg_id: 1,
        body_len: 0,
        encryption_offset: 0,
        status_class: make_status(BC_CLASS_MODERN_SHORT, 0),
        extension: None,
    };
    assert!(h.is_modern());
    assert!(!h.is_binary());
}

#[test]
fn test_header_is_extended() {
    let h = PacketHeader {
        msg_id: 1,
        body_len: 0,
        encryption_offset: 0,
        status_class: make_status(BC_CLASS_MODERN_EXT, 0),
        extension: Some(0),
    };
    assert!(h.is_extended());
}

#[test]
fn test_header_is_encrypted() {
    // Encrypted: offset < body_len
    let h = PacketHeader {
        msg_id: 1,
        body_len: 100,
        encryption_offset: 0,
        status_class: 0,
        extension: None,
    };
    assert!(h.is_encrypted());

    // Not encrypted: offset == body_len
    let h2 = PacketHeader {
        msg_id: 1,
        body_len: 100,
        encryption_offset: 100,
        status_class: 0,
        extension: None,
    };
    assert!(!h2.is_encrypted());
}

#[test]
fn test_header_len() {
    let short = PacketHeader {
        msg_id: 1,
        body_len: 0,
        encryption_offset: 0,
        status_class: 0,
        extension: None,
    };
    assert_eq!(short.header_len(), 20);

    let extended = PacketHeader {
        msg_id: 1,
        body_len: 0,
        encryption_offset: 0,
        status_class: make_status(BC_CLASS_MODERN_EXT, 0),
        extension: Some(0),
    };
    assert_eq!(extended.header_len(), 24);
}

#[test]
fn test_header_serialize_known_bytes() {
    let h = PacketHeader {
        msg_id: 1,
        body_len: 0,
        encryption_offset: 0,
        status_class: 0,
        extension: None,
    };
    let mut buf = [0u8; HEADER_LEN_EXTENDED];
    let len = h.serialize(&mut buf);
    assert_eq!(len, 20);
    // First 4 bytes: magic (LE)
    assert_eq!(&buf[0..4], &[0xf0, 0xde, 0xbc, 0x0a]);
    // msg_id = 1 (LE)
    assert_eq!(&buf[4..8], &[0x01, 0x00, 0x00, 0x00]);
    // body_len, encryption_offset, status_class all zero
    assert_eq!(&buf[8..20], &[0u8; 12]);
}

#[test]
fn test_header_parse_with_trailing_data() {
    // Parse should only consume header bytes, ignoring trailing data
    let mut data = make_header_bytes(5, 0, 0, make_status(BC_CLASS_LEGACY, 0), None);
    data.extend_from_slice(&[0xAA, 0xBB, 0xCC]); // trailing
    let (header, consumed) = PacketHeader::parse(&data).unwrap();
    assert_eq!(consumed, 20);
    assert_eq!(header.msg_id, 5);
    // Trailing bytes are untouched
    assert_eq!(data.len(), 23);
}
