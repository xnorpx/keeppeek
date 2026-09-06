use isapi::{Decoder, PartKind};

fn payload() -> Vec<u8> {
    let mut payload = b"\r\n--camera\r\nContent-Type: application/xml; charset=\"UTF-8\"\r\nContent-Length: 5\r\n\r\n<ok/>\r\n--camera\r\nContent-Type: image/jpeg\r\nContent-Length: 4\r\nContent-ID: snapshot\r\n\r\n".to_vec();
    payload.extend_from_slice(&[0xff, 0xd8, 0xff, 0xd9]);
    payload.extend_from_slice(b"\r\n--camera--\r\n");
    payload
}

#[test]
fn every_network_split_preserves_xml_and_binary_parts() {
    let payload = payload();
    for chunk_size in 1..=payload.len() {
        let mut parser = Decoder::new("multipart/mixed; boundary=\"camera\"").unwrap();
        let mut parts = Vec::new();
        for chunk in payload.chunks(chunk_size) {
            parser.push(chunk).unwrap();
            while let Some(part) = parser.next_part().unwrap() {
                parts.push(part);
            }
        }
        parser.finish().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].kind(), PartKind::Xml);
        assert_eq!(parts[0].charset(), Some("utf-8"));
        assert_eq!(parts[0].body(), b"<ok/>");
        assert_eq!(parts[1].kind(), PartKind::Jpeg);
        assert_eq!(parts[1].body(), &[0xff, 0xd8, 0xff, 0xd9]);
        assert_eq!(parts[1].content_id(), Some("snapshot"));
    }
}

#[test]
fn lengthless_payload_uses_delimiter_lines_not_boundary_like_content() {
    let mut parser = Decoder::new("multipart/mixed; boundary=camera").unwrap();
    let expected = b"{\"note\":\"--camera\"}\r\n--camera-not-a-delimiter";
    let mut payload = b"--camera\r\nContent-Type: application/json\r\n\r\n".to_vec();
    payload.extend_from_slice(expected);
    payload.extend_from_slice(b"\r\n--camera--\r\n");
    for byte in payload {
        parser.push([byte]).unwrap();
    }
    let part = parser.next_part().unwrap().unwrap();
    assert_eq!(part.kind(), PartKind::Json);
    assert_eq!(part.body(), expected);
    assert!(parser.next_part().unwrap().is_none());
    parser.finish().unwrap();
}

#[test]
fn rejects_bad_mime_and_ambiguous_or_oversized_headers() {
    for mime in [
        "application/xml",
        "multipart/mixed",
        "multipart/mixed; boundary=\"\"",
        "multipart/mixed; boundary=one; boundary=two",
    ] {
        assert!(Decoder::new(mime).is_err());
    }
    for headers in [
        "Content-Type: application/xml\r\nContent-Length: 1\r\nContent-Length: 2\r\n".to_owned(),
        "Content-Type: application/xml\r\nContent-Length: 262145\r\n".to_owned(),
        "Content-Type: image/jpeg\r\nContent-Length: 1048577\r\n".to_owned(),
        format!(
            "Content-Type: application/xml\r\nX-Long: {}\r\n",
            "a".repeat(8192)
        ),
    ] {
        let mut parser = Decoder::new("multipart/mixed; boundary=camera").unwrap();
        parser
            .push(format!("--camera\r\n{headers}\r\n").as_bytes())
            .unwrap();
        assert!(parser.next_part().is_err());
        assert!(parser.push(b"more").is_err());
    }
}

#[test]
fn disconnect_during_a_part_is_not_a_clean_end() {
    let mut parser = Decoder::new("multipart/mixed; boundary=camera").unwrap();
    parser
        .push(b"--camera\r\nContent-Type: application/xml\r\nContent-Length: 5\r\n\r\n<ok")
        .unwrap();
    assert!(parser.next_part().unwrap().is_none());
    assert!(parser.finish().is_err());
}

#[test]
fn resource_limits_apply_even_when_caller_does_not_drain_parts() {
    let mut parser = Decoder::new("multipart/mixed; boundary=camera").unwrap();
    assert!(parser.push(vec![0; 65537]).unwrap_err().is_limit());
}
