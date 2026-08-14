use xml::reader::{Error as XmlError, XmlEvent};

pub fn assert_xml_eq(actual: &str, expected: &str) {
    for (a, e) in without_whitespaces(actual).zip(without_whitespaces(expected)) {
        // Compare StartDocument events case-insensitively for encoding (UTF-8 vs utf-8)
        match (&a, &e) {
            (
                Ok(XmlEvent::StartDocument {
                    encoding: enc_a, ..
                }),
                Ok(XmlEvent::StartDocument {
                    encoding: enc_e, ..
                }),
            ) => {
                assert_eq!(
                    enc_a.to_ascii_uppercase(),
                    enc_e.to_ascii_uppercase(),
                    "XML encoding mismatch"
                );
            }
            _ => assert_eq!(a, e),
        }
    }
}

fn without_whitespaces(expected: &str) -> impl Iterator<Item = Result<XmlEvent, XmlError>> + '_ {
    xml::EventReader::new(expected.as_bytes())
        .into_iter()
        .filter(|e| !matches!(e, Ok(XmlEvent::Whitespace(_))))
}
