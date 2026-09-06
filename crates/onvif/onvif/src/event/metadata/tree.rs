//! Restore qualified attributes that xmltree otherwise reduces to local names.

use ::xml::reader::{ParserConfig, XmlEvent};
use xmltree::{Element, XMLNode};

use super::{ProtocolError, SCHEMA, STRING_SIZE_BYTES_MAX, XML_SIZE_BYTES_MAX, xml};

const ATTRIBUTE_SIZE_BYTES_MAX: usize = 4096;
const TEXT_SIZE_BYTES_MAX: usize = 64 * 1024;

pub(super) fn parse(bytes: &[u8]) -> Result<Element, ProtocolError> {
    let mut root = xml::parse(bytes, XML_SIZE_BYTES_MAX)?;
    let mut pending = vec![&mut root];
    let reader = ParserConfig::new()
        .max_name_length(256)
        .max_attributes(32)
        .max_attribute_length(ATTRIBUTE_SIZE_BYTES_MAX)
        .max_data_length(TEXT_SIZE_BYTES_MAX)
        .max_entity_expansion_depth(1)
        .max_entity_expansion_length(4096)
        .create_reader(bytes);
    for event in reader {
        let XmlEvent::StartElement {
            name, attributes, ..
        } = event.map_err(|_| ProtocolError("invalid metadata XML"))?
        else {
            continue;
        };
        let element = pending
            .pop()
            .ok_or(ProtocolError("inconsistent metadata XML tree"))?;
        if element.name != name.local_name {
            return Err(ProtocolError("inconsistent metadata XML tree"));
        }
        if element.namespace != name.namespace {
            return Err(ProtocolError("inconsistent metadata XML tree"));
        }
        element.attributes = attributes
            .into_iter()
            .map(|attribute| {
                let key = match attribute.name.prefix {
                    Some(prefix) => format!("{prefix}:{}", attribute.name.local_name),
                    None => attribute.name.local_name,
                };
                (key, attribute.value)
            })
            .collect();
        validate_sizes(element)?;
        pending.extend(
            element
                .children
                .iter_mut()
                .rev()
                .filter_map(XMLNode::as_mut_element),
        );
    }
    if !pending.is_empty() {
        return Err(ProtocolError("incomplete metadata XML tree"));
    }
    Ok(root)
}

fn validate_sizes(element: &Element) -> Result<(), ProtocolError> {
    let text_bytes: usize = element
        .children
        .iter()
        .filter_map(|node| match node {
            XMLNode::Text(text) | XMLNode::CData(text) => Some(text.len()),
            _ => None,
        })
        .sum();
    if text_bytes > TEXT_SIZE_BYTES_MAX {
        return Err(ProtocolError("metadata XML text limit exceeded"));
    }
    if element
        .attributes
        .values()
        .any(|value| value.len() > ATTRIBUTE_SIZE_BYTES_MAX)
    {
        return Err(ProtocolError("metadata XML attribute limit exceeded"));
    }
    Ok(())
}

pub(super) fn children<'a>(
    parent: &'a Element,
    name: &'static str,
) -> impl Iterator<Item = &'a Element> + Clone {
    parent
        .children
        .iter()
        .filter_map(XMLNode::as_element)
        .filter(move |element| xml::matches(element, SCHEMA, name))
}

pub(super) fn bounded<'a, Value>(
    elements: impl Iterator<Item = &'a Element> + Clone,
    limit: usize,
    parse: impl Fn(&Element) -> Result<Value, ProtocolError>,
) -> Result<Vec<Value>, ProtocolError> {
    let count = elements.clone().count();
    if count > limit {
        return Err(ProtocolError("metadata count limit exceeded"));
    }
    let mut values = Vec::with_capacity(count);
    for element in elements {
        values.push(parse(element)?);
    }
    Ok(values)
}

pub(super) fn attribute<'a>(element: &'a Element, name: &str) -> Result<&'a str, ProtocolError> {
    element
        .attributes
        .get(name)
        .map(String::as_str)
        .ok_or(ProtocolError("missing metadata attribute"))
}

pub(super) fn string(value: &str) -> Result<String, ProtocolError> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > STRING_SIZE_BYTES_MAX
        || value.chars().any(char::is_control)
    {
        return Err(ProtocolError("invalid metadata string"));
    }
    Ok(value.to_owned())
}

pub(super) fn scalar(element: &Element) -> Result<String, ProtocolError> {
    string(&xml::text(element)?)
}

pub(super) fn likelihood(element: &Element) -> Result<Option<f32>, ProtocolError> {
    element
        .attributes
        .get("Likelihood")
        .map(|value| probability(value))
        .transpose()
}

pub(super) fn probability(value: &str) -> Result<f32, ProtocolError> {
    let value = value.trim();
    let wide = value
        .parse::<f64>()
        .map_err(|_| ProtocolError("invalid metadata likelihood"))?;
    if !wide.is_finite() || !(0.0..=1.0).contains(&wide) {
        return Err(ProtocolError("metadata likelihood out of range"));
    }
    value
        .parse()
        .map_err(|_| ProtocolError("invalid metadata likelihood"))
}
