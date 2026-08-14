use arrayvec::ArrayString;
use reo_proto::xml::*;

#[test]
fn test_xml_build_and_parse_roundtrip() {
    let mut buf = [0u8; 512];
    let len = build_xml(&mut buf, |b| {
        b.start_versioned("DeviceInfo", "1");
        b.text_element("model", "RLC-810A");
        b.u32_element("channelNum", 2);
        b.end();
    })
    .unwrap();

    let mut model = ArrayString::<64>::new();
    let mut channels = None;
    parse_xml(&buf[..len], |name, text| match name {
        "model" => {
            if let Ok(s) = ArrayString::try_from(text) {
                model = s;
            }
        }
        "channelNum" => channels = text.parse().ok(),
        _ => {}
    })
    .unwrap();

    assert_eq!(model.as_str(), "RLC-810A");
    assert_eq!(channels, Some(2));
}

#[test]
fn test_xml_extract_text() {
    let xml = b"<body><nonce>ABC123</nonce></body>";
    let mut out = ArrayString::<64>::new();
    let found = extract_text(xml.as_slice(), "nonce", &mut out).unwrap();
    assert!(found);
    assert_eq!(out.as_str(), "ABC123");
}

#[test]
fn test_xml_extract_u32() {
    let xml = b"<body><port>9000</port></body>";
    let val = extract_u32(xml.as_slice(), "port").unwrap();
    assert_eq!(val, Some(9000));
}

#[test]
fn test_xml_extract_missing() {
    let xml = b"<body><other>val</other></body>";
    let val = extract_u32(xml.as_slice(), "port").unwrap();
    assert_eq!(val, None);
}
