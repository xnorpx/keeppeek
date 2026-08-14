use crate::error::BcError;
use arrayvec::ArrayString;
use std::io::Cursor;
use xml::{
    reader::{EventReader, XmlEvent as ReadEvent},
    writer::{EmitterConfig, EventWriter, XmlEvent as WriteEvent},
};

/// Write a Baichuan XML `<body>` document into a caller-provided buffer.
///
/// The `build` closure receives an `XmlBuilder` which provides methods to
/// write elements and text. Returns the number of bytes written.
///
/// # Errors
///
/// Returns `BcError::XmlParse` if writing fails.
pub fn build_xml(
    buf: &mut [u8],
    build: impl FnOnce(&mut XmlBuilder<'_>),
) -> Result<usize, BcError> {
    let mut builder = XmlBuilder::new(buf)?;
    builder.raw_start("body")?;
    build(&mut builder);
    builder.raw_end()?;
    builder.finish()
}

pub(crate) fn build_versioned_document(
    buf: &mut [u8],
    root: &str,
    version: &str,
    build: impl FnOnce(&mut XmlBuilder<'_>),
) -> Result<usize, BcError> {
    let mut builder = XmlBuilder::new(buf)?;
    builder.start_versioned(root, version);
    build(&mut builder);
    builder.end();
    builder.finish()
}

/// Helper for building XML elements inside a `<body>` document.
pub struct XmlBuilder<'buf> {
    writer: EventWriter<Cursor<&'buf mut [u8]>>,
    err: bool,
}

impl<'buf> XmlBuilder<'buf> {
    fn new(buf: &'buf mut [u8]) -> Result<Self, BcError> {
        let cursor = Cursor::new(buf);
        let config = EmitterConfig::new()
            .write_document_declaration(false)
            .perform_indent(false);
        let writer = config.create_writer(cursor);
        Ok(XmlBuilder { writer, err: false })
    }

    fn raw_start(&mut self, name: &str) -> Result<(), BcError> {
        self.writer
            .write(WriteEvent::start_element(name))
            .map_err(|_| BcError::XmlParse("failed to write start element"))
    }

    fn raw_end(&mut self) -> Result<(), BcError> {
        self.writer
            .write(WriteEvent::end_element())
            .map_err(|_| BcError::XmlParse("failed to write end element"))
    }

    fn finish(self) -> Result<usize, BcError> {
        if self.err {
            return Err(BcError::XmlParse("xml write error during build"));
        }
        Ok(self.writer.into_inner().position() as usize)
    }

    /// Start an element with a version attribute: `<name version="ver">`.
    pub fn start_versioned(&mut self, name: &str, version: &str) {
        if self
            .writer
            .write(WriteEvent::start_element(name).attr("version", version))
            .is_err()
        {
            self.err = true;
        }
    }

    /// Start a plain element: `<name>`.
    pub fn start(&mut self, name: &str) {
        if self.writer.write(WriteEvent::start_element(name)).is_err() {
            self.err = true;
        }
    }

    /// End the current element: `</...>`.
    pub fn end(&mut self) {
        if self.writer.write(WriteEvent::end_element()).is_err() {
            self.err = true;
        }
    }

    /// Write a text-only element: `<name>text</name>`.
    pub fn text_element(&mut self, name: &str, text: &str) {
        self.start(name);
        if self.writer.write(WriteEvent::characters(text)).is_err() {
            self.err = true;
        }
        self.end();
    }

    /// Write a u32 element: `<name>123</name>`.
    pub fn u32_element(&mut self, name: &str, value: u32) {
        let mut num_buf = [0u8; 10];
        let s = write_u32(value, &mut num_buf);
        self.text_element(name, s);
    }

    /// Write a u8 element: `<name>5</name>`.
    pub fn u8_element(&mut self, name: &str, value: u8) {
        self.u32_element(name, value as u32);
    }
}

/// Format a u32 into a stack buffer and return the str slice.
fn write_u32(value: u32, buf: &mut [u8; 10]) -> &str {
    if value == 0 {
        return "0";
    }
    let mut pos = buf.len();
    let mut v = value;
    while v > 0 {
        pos -= 1;
        buf[pos] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    // Safety: we only wrote ASCII digits
    std::str::from_utf8(&buf[pos..]).unwrap()
}

/// Parse a Baichuan XML `<body>` document from bytes.
///
/// The `visitor` closure is called for each element with its name and
/// text content. This allows allocation-free extraction of fields.
pub fn parse_xml(data: &[u8], mut visitor: impl FnMut(&str, &str)) -> Result<(), BcError> {
    let reader = EventReader::new(data);
    let mut current_element: Option<ArrayString<64>> = None;

    for event in reader {
        match event {
            Ok(ReadEvent::StartElement { name, .. }) => {
                current_element = ArrayString::try_from(name.local_name.as_str()).ok();
            }
            Ok(ReadEvent::Characters(text)) | Ok(ReadEvent::CData(text)) => {
                if let Some(ref elem) = current_element {
                    visitor(elem.as_str(), &text);
                }
            }
            Ok(ReadEvent::EndElement { .. }) => {
                current_element = None;
            }
            Ok(ReadEvent::EndDocument) => break,
            Err(_) => return Err(BcError::XmlParse("malformed XML")),
            _ => {}
        }
    }

    Ok(())
}

pub(crate) enum XmlVisit<'a> {
    Start(&'a str),
    Text { name: &'a str, text: &'a str },
    End(&'a str),
}

pub(crate) fn visit_xml(data: &[u8], mut visitor: impl FnMut(XmlVisit<'_>)) -> Result<(), BcError> {
    let reader = EventReader::new(data);
    let mut current_element: Option<ArrayString<64>> = None;

    for event in reader {
        match event {
            Ok(ReadEvent::StartElement { name, .. }) => {
                visitor(XmlVisit::Start(name.local_name.as_str()));
                current_element = ArrayString::try_from(name.local_name.as_str()).ok();
            }
            Ok(ReadEvent::Characters(text)) | Ok(ReadEvent::CData(text)) => {
                if let Some(ref name) = current_element {
                    visitor(XmlVisit::Text {
                        name: name.as_str(),
                        text: &text,
                    });
                }
            }
            Ok(ReadEvent::EndElement { name }) => {
                visitor(XmlVisit::End(name.local_name.as_str()));
                current_element = None;
            }
            Ok(ReadEvent::EndDocument) => break,
            Err(_) => return Err(BcError::XmlParse("malformed XML")),
            _ => {}
        }
    }

    Ok(())
}

/// Parse a Baichuan XML document and extract element text into an `ArrayString`.
///
/// A convenience wrapper that matches a specific element name and copies its
/// text into the provided `ArrayString`. Returns whether the element was found.
pub fn extract_text<const N: usize>(
    data: &[u8],
    element_name: &str,
    out: &mut ArrayString<N>,
) -> Result<bool, BcError> {
    let mut found = false;
    parse_xml(data, |name, text| {
        if name == element_name
            && let Ok(s) = ArrayString::<N>::try_from(text)
        {
            *out = s;
            found = true;
        }
    })?;
    Ok(found)
}

/// Parse a Baichuan XML document and extract element text as a u32.
pub fn extract_u32(data: &[u8], element_name: &str) -> Result<Option<u32>, BcError> {
    let mut result = None;
    parse_xml(data, |name, text| {
        if name == element_name
            && let Ok(v) = text.parse::<u32>()
        {
            result = Some(v);
        }
    })?;
    Ok(result)
}

/// Parse XML and visit elements, providing version attributes when present.
///
/// The visitor receives `(element_name, text, optional_version)`.
pub fn parse_xml_versioned(
    data: &[u8],
    element_name: &str,
    mut visitor: impl FnMut(&str, &str, Option<&str>),
) -> Result<(), BcError> {
    let reader = EventReader::new(data);
    let mut current_element: Option<ArrayString<64>> = None;
    let mut target_version: Option<ArrayString<16>> = None;
    let mut target_depth: usize = 0;
    let mut depth: usize = 0;

    for event in reader {
        match event {
            Ok(ReadEvent::StartElement {
                name, attributes, ..
            }) => {
                depth += 1;
                current_element = ArrayString::try_from(name.local_name.as_str()).ok();
                if name.local_name == element_name {
                    target_depth = depth;
                    target_version = None;
                    for attr in &attributes {
                        if attr.name.local_name == "version" {
                            target_version = ArrayString::try_from(attr.value.as_str()).ok();
                        }
                    }
                }
            }
            Ok(ReadEvent::Characters(text)) | Ok(ReadEvent::CData(text)) => {
                if let Some(ref elem) = current_element {
                    let ver = if target_depth > 0 {
                        target_version.as_deref()
                    } else {
                        None
                    };
                    visitor(elem.as_str(), &text, ver);
                }
            }
            Ok(ReadEvent::EndElement { name, .. }) => {
                if depth == target_depth && name.local_name == element_name {
                    target_version = None;
                    target_depth = 0;
                }
                depth = depth.saturating_sub(1);
                current_element = None;
            }
            Ok(ReadEvent::EndDocument) => break,
            Err(_) => return Err(BcError::XmlParse("malformed XML")),
            _ => {}
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_simple_xml() {
        let mut buf = [0u8; 256];
        let len = build_xml(&mut buf, |b| {
            b.start_versioned("Preview", "1.1");
            b.u32_element("channelId", 0);
            b.u32_element("handle", 0);
            b.text_element("streamType", "mainStream");
            b.end();
        })
        .unwrap();

        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<body>"));
        assert!(xml.contains("</body>"));
        assert!(xml.contains("<Preview version=\"1.1\">"));
        assert!(xml.contains("<channelId>0</channelId>"));
        assert!(xml.contains("<streamType>mainStream</streamType>"));
    }

    #[test]
    fn build_login_xml() {
        let mut buf = [0u8; 512];
        let len = build_xml(&mut buf, |b| {
            b.start_versioned("LoginUser", "2");
            b.text_element("userName", "admin");
            b.text_element("password", "ABCDEF1234567890ABCDEF12345678A");
            b.u32_element("userVer", 1);
            b.end();
            b.start_versioned("LoginNet", "2");
            b.text_element("type", "LAN");
            b.u32_element("udpPort", 0);
            b.end();
        })
        .unwrap();

        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<LoginUser version=\"2\">"));
        assert!(xml.contains("<userName>admin</userName>"));
        assert!(xml.contains("<LoginNet version=\"2\">"));
    }

    #[test]
    fn parse_nonce_xml() {
        let xml = br#"<body><Encryption version="2"><type>aes</type><nonce>ABCDEF123456</nonce></Encryption></body>"#;
        let mut nonce = ArrayString::<64>::new();
        let mut enc_type = ArrayString::<16>::new();

        parse_xml(xml.as_slice(), |name, text| match name {
            "nonce" => {
                if let Ok(s) = ArrayString::<64>::try_from(text) {
                    nonce = s;
                }
            }
            "type" => {
                if let Ok(s) = ArrayString::<16>::try_from(text) {
                    enc_type = s;
                }
            }
            _ => {}
        })
        .unwrap();

        assert_eq!(nonce.as_str(), "ABCDEF123456");
        assert_eq!(enc_type.as_str(), "aes");
    }

    #[test]
    fn parse_login_confirmation() {
        let xml = br#"<body><LoginUser version="2"><userName>admin</userName><result>ok</result><userId>123</userId></LoginUser><DeviceInfo version="2"><model>RLC-811A</model><serialNumber>SN12345</serialNumber><channelNum>1</channelNum></DeviceInfo></body>"#;

        let mut user_id = None;
        let mut model = ArrayString::<64>::new();
        let mut serial = ArrayString::<64>::new();

        parse_xml(xml.as_slice(), |name, text| match name {
            "userId" => user_id = text.parse().ok(),
            "model" => {
                if let Ok(s) = ArrayString::try_from(text) {
                    model = s;
                }
            }
            "serialNumber" => {
                if let Ok(s) = ArrayString::try_from(text) {
                    serial = s;
                }
            }
            _ => {}
        })
        .unwrap();

        assert_eq!(user_id, Some(123));
        assert_eq!(model.as_str(), "RLC-811A");
        assert_eq!(serial.as_str(), "SN12345");
    }

    #[test]
    fn extract_text_helper() {
        let xml = br#"<body><nonce>DEADBEEF</nonce></body>"#;
        let mut out = ArrayString::<64>::new();
        let found = extract_text(xml.as_slice(), "nonce", &mut out).unwrap();
        assert!(found);
        assert_eq!(out.as_str(), "DEADBEEF");
    }

    #[test]
    fn extract_u32_helper() {
        let xml = br#"<body><channelNum>4</channelNum></body>"#;
        let result = extract_u32(xml.as_slice(), "channelNum").unwrap();
        assert_eq!(result, Some(4));
    }

    #[test]
    fn extract_missing_element() {
        let xml = br#"<body><other>value</other></body>"#;
        let mut out = ArrayString::<64>::new();
        let found = extract_text(xml.as_slice(), "nonce", &mut out).unwrap();
        assert!(!found);
    }

    #[test]
    fn build_roundtrip() {
        let mut buf = [0u8; 256];
        let len = build_xml(&mut buf, |b| {
            b.start_versioned("Test", "1");
            b.text_element("name", "hello");
            b.u32_element("value", 42);
            b.end();
        })
        .unwrap();

        let mut name_out = ArrayString::<64>::new();
        let found = extract_text(&buf[..len], "name", &mut name_out).unwrap();
        assert!(found);
        assert_eq!(name_out.as_str(), "hello");

        let value = extract_u32(&buf[..len], "value").unwrap();
        assert_eq!(value, Some(42));
    }

    #[test]
    fn parse_versioned_extracts_version() {
        let xml = br#"<body><Encryption version="2"><type>aes</type><nonce>ABC</nonce></Encryption></body>"#;
        let mut version_seen = ArrayString::<16>::new();
        let mut nonce = ArrayString::<64>::new();

        parse_xml_versioned(xml.as_slice(), "Encryption", |name, text, version| {
            if let Some(v) = version
                && let Ok(s) = ArrayString::try_from(v)
            {
                version_seen = s;
            }
            if name == "nonce"
                && let Ok(s) = ArrayString::try_from(text)
            {
                nonce = s;
            }
        })
        .unwrap();

        assert_eq!(version_seen.as_str(), "2");
        assert_eq!(nonce.as_str(), "ABC");
    }

    #[test]
    fn parse_versioned_no_version_attribute() {
        let xml = br#"<body><Info><name>test</name></Info></body>"#;
        let mut got_version = false;

        parse_xml_versioned(xml.as_slice(), "Info", |_name, _text, version| {
            if version.is_some() {
                got_version = true;
            }
        })
        .unwrap();

        assert!(!got_version);
    }

    #[test]
    fn parse_versioned_only_matches_target_element() {
        let xml = br#"<body><A version="1"><x>1</x></A><B version="2"><y>2</y></B></body>"#;
        let mut version_for_b = ArrayString::<16>::new();

        parse_xml_versioned(xml.as_slice(), "B", |_name, _text, version| {
            if let Some(v) = version
                && let Ok(s) = ArrayString::try_from(v)
            {
                version_for_b = s;
            }
        })
        .unwrap();

        assert_eq!(version_for_b.as_str(), "2");
    }

    #[test]
    fn build_empty_body() {
        let mut buf = [0u8; 64];
        let len = build_xml(&mut buf, |_b| {
            // no elements
        })
        .unwrap();
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        // xml-rs may produce <body></body> or <body />
        assert!(xml.contains("body"));
        assert!(len < 30);
    }

    #[test]
    fn build_special_xml_characters() {
        let mut buf = [0u8; 512];
        let len = build_xml(&mut buf, |b| {
            b.text_element("data", "a < b & c > d \"e\" 'f'");
        })
        .unwrap();
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        // xml-rs should escape special characters
        assert!(xml.contains("&lt;"));
        assert!(xml.contains("&amp;"));
        assert!(xml.contains("&gt;"));
    }

    #[test]
    fn build_u32_max() {
        let mut buf = [0u8; 128];
        let len = build_xml(&mut buf, |b| {
            b.u32_element("max", u32::MAX);
        })
        .unwrap();
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains(&format!("<max>{}</max>", u32::MAX)));
    }

    #[test]
    fn build_u32_zero() {
        let mut buf = [0u8; 128];
        let len = build_xml(&mut buf, |b| {
            b.u32_element("zero", 0);
        })
        .unwrap();
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<zero>0</zero>"));
    }

    #[test]
    fn build_u8_element() {
        let mut buf = [0u8; 128];
        let len = build_xml(&mut buf, |b| {
            b.u8_element("channel", 255);
        })
        .unwrap();
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<channel>255</channel>"));
    }

    #[test]
    fn parse_malformed_xml() {
        let xml = b"<body><unclosed>";
        // Should not panic; returns error or visits what it can
        let result = parse_xml(xml.as_slice(), |_name, _text| {});
        // malformed XML should produce an error
        assert!(result.is_err());
    }

    #[test]
    fn parse_empty_element_text() {
        let xml = b"<body><empty></empty></body>";
        let mut found = false;
        parse_xml(xml.as_slice(), |name, _text| {
            if name == "empty" {
                found = true;
            }
        })
        .unwrap();
        // Empty element has no Characters event, so visitor won't be called with "empty"
        assert!(!found);
    }

    #[test]
    fn build_nested_elements() {
        let mut buf = [0u8; 512];
        let len = build_xml(&mut buf, |b| {
            b.start("outer");
            b.start("inner");
            b.text_element("leaf", "value");
            b.end(); // inner
            b.end(); // outer
        })
        .unwrap();
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<outer>"));
        assert!(xml.contains("<inner>"));
        assert!(xml.contains("<leaf>value</leaf>"));
        assert!(xml.contains("</inner>"));
        assert!(xml.contains("</outer>"));
    }

    #[test]
    fn extract_text_capacity_overflow() {
        // ArrayString<4> can't hold "toolong"
        let xml = b"<body><val>toolong</val></body>";
        let mut out = ArrayString::<4>::new();
        let found = extract_text(xml.as_slice(), "val", &mut out).unwrap();
        // Should not find because text doesn't fit in ArrayString<4>
        assert!(!found);
        assert!(out.is_empty());
    }
}
