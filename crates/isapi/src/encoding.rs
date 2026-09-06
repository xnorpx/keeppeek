use std::borrow::Cow;

use encoding_rs::{Encoding, GB18030, GBK, UTF_8, UTF_16BE, UTF_16LE};
use xml::reader::{ParserConfig, XmlEvent};

use crate::error::Kind;
use crate::{Error, XML_SIZE_BYTES_MAX};

pub const TEXT_SIZE_BYTES_MAX: usize = 1024 * 1024;
const DECLARATION_SIZE_BYTES_MAX: usize = 1024;

pub fn xml(bytes: &[u8], charset: Option<&str>) -> Result<String, Error> {
    check_size(bytes)?;
    let (marked, skip) =
        Encoding::for_bom(bytes).map_or((None, 0), |(codec, skip)| (Some(codec), skip));
    let bytes = &bytes[skip..];
    let inferred = marked.or_else(|| {
        if bytes.starts_with(b"\0<\0?") {
            Some(UTF_16BE)
        } else if bytes.starts_with(b"<\0?\0") {
            Some(UTF_16LE)
        } else {
            None
        }
    });
    let external = charset.map(|label| codec(label, inferred)).transpose()?;
    consistent(inferred, external)?;
    let prefix = if let Some(codec) = inferred {
        decode(bytes, codec)?
    } else {
        Cow::Borrowed("")
    };
    let declaration = declaration(if inferred.is_some() {
        prefix.as_bytes()
    } else {
        bytes
    })?;
    let declared = declaration
        .as_ref()
        .and_then(|value| value.encoding.as_deref())
        .map(|label| codec(label, inferred.or(external)))
        .transpose()?;
    consistent(inferred, declared)?;
    consistent(external, declared)?;
    let selected = inferred.or(external).or(declared).unwrap_or(UTF_8);
    let decoded = if inferred.is_some() {
        prefix
    } else {
        decode(bytes, selected)?
    };
    if (charset.is_some_and(ascii)
        || declaration
            .as_ref()
            .and_then(|decl| decl.encoding.as_deref())
            .is_some_and(ascii))
        && !decoded.is_ascii()
    {
        return Err(Error::new(Kind::Protocol));
    }
    let Some(declaration) = declaration else {
        return Ok(decoded.into_owned());
    };
    let mut output = Vec::new();
    xml::writer::EmitterConfig::new()
        .create_writer(&mut output)
        .write(xml::writer::XmlEvent::StartDocument {
            version: declaration.version,
            encoding: Some("UTF-8"),
            standalone: declaration.standalone,
        })
        .map_err(|_| Error::new(Kind::Protocol))?;
    output.extend_from_slice(&decoded.as_bytes()[declaration.end..]);
    if output.len() > TEXT_SIZE_BYTES_MAX {
        return Err(Error::new(Kind::Limit));
    }
    String::from_utf8(output).map_err(|_| Error::new(Kind::Protocol))
}

pub fn json<'input>(bytes: &'input [u8], charset: Option<&str>) -> Result<Cow<'input, str>, Error> {
    check_size(bytes)?;
    let (marked, skip) =
        Encoding::for_bom(bytes).map_or((None, 0), |(codec, skip)| (Some(codec), skip));
    let external = charset.map(|label| codec(label, marked)).transpose()?;
    consistent(marked, external)?;
    let decoded = decode(&bytes[skip..], marked.or(external).unwrap_or(UTF_8))?;
    if charset.is_some_and(ascii) && !decoded.is_ascii() {
        return Err(Error::new(Kind::Protocol));
    }
    Ok(decoded)
}

const fn check_size(bytes: &[u8]) -> Result<(), Error> {
    if bytes.len() > XML_SIZE_BYTES_MAX {
        return Err(Error::new(Kind::Limit));
    }
    Ok(())
}

fn ascii(label: &str) -> bool {
    label.eq_ignore_ascii_case("us-ascii") || label.eq_ignore_ascii_case("ascii")
}

fn codec(label: &str, evidence: Option<&'static Encoding>) -> Result<&'static Encoding, Error> {
    if ascii(label) {
        return Ok(UTF_8);
    }
    if label.eq_ignore_ascii_case("utf-16") {
        return evidence
            .filter(|codec| *codec == UTF_16LE || *codec == UTF_16BE)
            .ok_or_else(|| Error::new(Kind::Protocol));
    }
    Encoding::for_label_no_replacement(label.as_bytes())
        .filter(|codec| [UTF_8, UTF_16LE, UTF_16BE, GBK, GB18030].contains(codec))
        .ok_or_else(|| Error::new(Kind::Protocol))
}

fn consistent(first: Option<&Encoding>, second: Option<&Encoding>) -> Result<(), Error> {
    if first
        .zip(second)
        .is_some_and(|(first, second)| first != second)
    {
        return Err(Error::new(Kind::Protocol));
    }
    Ok(())
}

fn decode<'input>(
    bytes: &'input [u8],
    codec: &'static Encoding,
) -> Result<Cow<'input, str>, Error> {
    let text = codec
        .decode_without_bom_handling_and_without_replacement(bytes)
        .ok_or_else(|| Error::new(Kind::Protocol))?;
    if text.len() > TEXT_SIZE_BYTES_MAX {
        return Err(Error::new(Kind::Limit));
    }
    Ok(text)
}

struct Declaration {
    encoding: Option<String>,
    version: xml::common::XmlVersion,
    standalone: Option<bool>,
    end: usize,
}

fn declaration(bytes: &[u8]) -> Result<Option<Declaration>, Error> {
    if !bytes.starts_with(b"<?xml") || !bytes.get(5).is_some_and(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let end = bytes[..bytes.len().min(DECLARATION_SIZE_BYTES_MAX)]
        .windows(2)
        .position(|pair| pair == b"?>")
        .map(|index| index + 2)
        .ok_or_else(|| Error::new(Kind::Limit))?;
    let prolog = &bytes[..end];
    if !prolog.is_ascii() {
        return Err(Error::new(Kind::Protocol));
    }
    let mut parser = ParserConfig::new()
        .override_encoding(Some(xml::Encoding::Utf8))
        .ignore_invalid_encoding_declarations(true)
        .max_attribute_length(DECLARATION_SIZE_BYTES_MAX)
        .create_reader(prolog);
    match parser.next()? {
        XmlEvent::StartDocument {
            version,
            encoding,
            standalone,
        } => Ok(Some(Declaration {
            encoding: prolog
                .windows(b"encoding".len())
                .any(|part| part == b"encoding")
                .then_some(encoding),
            version,
            standalone,
            end,
        })),
        _ => Err(Error::new(Kind::Protocol)),
    }
}
