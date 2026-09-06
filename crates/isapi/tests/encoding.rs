use isapi::Event;

fn part(content_type: &str, body: &[u8]) -> isapi::Part {
    let mut bytes = format!(
        "--encoding\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    bytes.extend_from_slice(b"\r\n--encoding--\r\n");
    let mut decoder = isapi::Decoder::new("multipart/mixed; boundary=encoding").unwrap();
    let mut result = None;
    for chunk in bytes.chunks(7) {
        decoder.push(chunk).unwrap();
        if let Some(part) = decoder.next_part().unwrap() {
            result = Some(part);
        }
    }
    decoder.finish().unwrap();
    result.unwrap()
}

fn xml(encoding: &str, name: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"{encoding}\"?><EventNotificationAlert><eventType>VMD</eventType><eventState>active</eventState><channelName>{name}</channelName></EventNotificationAlert>"
    )
}

#[test]
fn gb2312_channel_name_is_decoded_without_replacement() {
    let mut bytes = xml("GB2312", "").into_bytes();
    let offset = bytes
        .windows(b"</channelName>".len())
        .position(|part| part == b"</channelName>")
        .unwrap();
    bytes.splice(offset..offset, [0xc8, 0xcb]);
    let event = Event::parse(bytes).unwrap();
    assert_eq!(event.channel_name(), Some("\u{4eba}"));
}

#[test]
fn utf16_byte_order_marks_and_surrogate_pairs_are_decoded() {
    for little in [false, true] {
        let mut bytes = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in xml("UTF-16", "Gate \u{1f6aa}").encode_utf16() {
            bytes.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        assert_eq!(
            Event::parse(bytes).unwrap().channel_name(),
            Some("Gate \u{1f6aa}")
        );
    }
}

#[test]
fn invalid_legacy_sequences_and_bom_conflicts_are_rejected() {
    let mut malformed = xml("GBK", "").into_bytes();
    let offset = malformed
        .windows(b"</channelName>".len())
        .position(|part| part == b"</channelName>")
        .unwrap();
    malformed.insert(offset, 0x81);
    assert!(Event::parse(malformed).is_err());
    let mut conflict = vec![0xff, 0xfe];
    for unit in xml("UTF-8", "gate").encode_utf16() {
        conflict.extend_from_slice(&unit.to_le_bytes());
    }
    assert!(Event::parse(conflict).is_err());
}

#[test]
fn multipart_legacy_xml_and_json_use_the_declared_charset() {
    let text = xml("GB18030", "Gate \u{1f6aa}");
    let (encoded, _, errors) = encoding_rs::GB18030.encode(&text);
    assert!(!errors);
    let event = part("application/xml; charset=GB18030", &encoded)
        .event()
        .unwrap()
        .unwrap();
    assert_eq!(event.channel_name(), Some("Gate \u{1f6aa}"));
    assert!(
        part("application/xml; charset=UTF-8", &encoded)
            .event()
            .is_err()
    );
    let text = "{\"eventType\":\"VMD\",\"eventState\":\"active\",\"channelName\":\"\u{4eba}\"}";
    let (encoded, _, errors) = encoding_rs::GBK.encode(text);
    assert!(!errors);
    assert_eq!(
        part("application/json; charset=gb2312", &encoded)
            .event()
            .unwrap()
            .unwrap()
            .channel_name(),
        Some("\u{4eba}")
    );
    assert!(
        part("application/json; charset=us-ascii", text.as_bytes())
            .event()
            .is_err()
    );
}

#[test]
fn declarations_cannot_hide_invalid_syntax_or_change_utf8_text() {
    for declaration in [
        "<?xml version='1.0' encoding='UTF-8' encoding='GBK'?>",
        "<?xml encoding='UTF-8' version='1.0'?>",
        "<?xml version='1.0' standalone='maybe'?>",
    ] {
        assert!(Event::parse(format!("{declaration}{}", xml("UTF-8", "name"))).is_err());
    }
    assert_eq!(
        Event::parse(xml("UTF-8", "\u{4eba}\u{1f6aa}"))
            .unwrap()
            .channel_name(),
        Some("\u{4eba}\u{1f6aa}")
    );
    assert!(Event::parse(xml("us-ascii", "\u{4eba}")).is_err());
    assert!(Event::parse(xml("UTF-16", "name")).is_err());
}

#[test]
fn encoded_size_limits_apply_before_conversion_and_invalid_utf16_fails() {
    let mut oversized = vec![b' '; isapi::XML_SIZE_BYTES_MAX + 1];
    oversized[..2].copy_from_slice(&[0xff, 0xfe]);
    assert!(Event::parse(oversized).unwrap_err().is_limit());
    for suffix in [vec![0x00, 0xd8], vec![0x00, 0xdc], vec![0x01]] {
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend(xml("UTF-16", "").encode_utf16().flat_map(u16::to_le_bytes));
        bytes.extend(suffix);
        assert!(Event::parse(bytes).is_err());
    }
}
