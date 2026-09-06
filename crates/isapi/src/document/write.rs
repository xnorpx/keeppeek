use super::{Content, Document, Element, Error, Kind, Node, XmlContent};
use crate::{Format, XML_SIZE_BYTES_MAX};
use serde_json::Value;

impl Document {
    pub(crate) fn empty_xml(root: &str) -> Result<Self, Error> {
        if root.is_empty() || !root.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return Err(Error::new(Kind::InvalidInput));
        }
        Self::parse_xml(
            format!("<{root} version=\"2.0\" xmlns=\"http://www.isapi.org/ver20/XMLSchema\"/>"),
            None,
        )
    }

    /// Returns the document's wire media format.
    pub const fn format(&self) -> Format {
        match self.content {
            Content::Xml(_) => Format::Xml,
            Content::Json(_) => Format::Json,
        }
    }

    /// Serializes the retained document with structured escaping and bounded output.
    ///
    /// # Errors
    /// Rejects output larger than the encoded request limit.
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let output = match &self.content {
            Content::Json(value) => serde_json::to_vec(value)?,
            Content::Xml(root) => encode_xml(root)?,
        };
        if output.len() > XML_SIZE_BYTES_MAX {
            return Err(Error::new(Kind::Limit));
        }
        Ok(output)
    }

    pub(crate) fn set(
        &mut self,
        root_name: &str,
        path: &[&str],
        value: Value,
    ) -> Result<(), Error> {
        if path.is_empty()
            || path.len() > 16
            || path.iter().any(|name| {
                name.is_empty() || !name.bytes().all(|byte| byte.is_ascii_alphanumeric())
            })
        {
            return Err(Error::new(Kind::InvalidInput));
        }
        self.root(root_name)?;
        let text = Node::Json(&value).text()?;
        match &mut self.content {
            Content::Json(root) => {
                let wrapped = root.get(root_name).is_some();
                let mut cursor = if wrapped {
                    root.get_mut(root_name)
                        .ok_or_else(|| Error::new(Kind::Protocol))?
                } else {
                    root
                };
                for name in &path[..path.len() - 1] {
                    let object = cursor
                        .as_object_mut()
                        .ok_or_else(|| Error::new(Kind::Protocol))?;
                    cursor = object
                        .entry((*name).to_owned())
                        .or_insert_with(|| Value::Object(serde_json::Map::new()));
                }
                cursor
                    .as_object_mut()
                    .ok_or_else(|| Error::new(Kind::Protocol))?
                    .insert(path[path.len() - 1].to_owned(), value);
            }
            Content::Xml(root) => {
                let mut cursor = root;
                for name in path {
                    let indices: Vec<_> = cursor
                        .content
                        .iter()
                        .enumerate()
                        .filter_map(|(index, item)| match item {
                            XmlContent::Element(child)
                                if child.namespace == cursor.namespace
                                    && child.name.rsplit(':').next() == Some(*name) =>
                            {
                                Some(index)
                            }
                            _ => None,
                        })
                        .collect();
                    if indices.len() > 1 {
                        return Err(Error::new(Kind::Protocol));
                    }
                    let index = if let Some(index) = indices.first() {
                        *index
                    } else {
                        let qualified = cursor.name.rsplit_once(':').map_or_else(
                            || (*name).to_owned(),
                            |(prefix, _)| format!("{prefix}:{name}"),
                        );
                        cursor.content.push(XmlContent::Element(Element {
                            name: qualified,
                            namespace: cursor.namespace.clone(),
                            namespaces: Default::default(),
                            attributes: Default::default(),
                            content: Vec::new(),
                        }));
                        cursor.content.len() - 1
                    };
                    let XmlContent::Element(child) = &mut cursor.content[index] else {
                        unreachable!("selected XML child is an element")
                    };
                    cursor = child;
                }
                cursor.content = vec![XmlContent::Text(text)];
            }
        }
        Ok(())
    }
}

fn encode_xml(root: &Element) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    let mut writer = xml::writer::EmitterConfig::new()
        .write_document_declaration(true)
        .create_writer(&mut bytes);
    let mut stack = vec![(Some(root), None)];
    while let Some((element, text)) = stack.pop() {
        let event = match (element, text) {
            (Some(element), _) => {
                let mut start = xml::writer::XmlEvent::start_element(element.name.as_str());
                for (prefix, uri) in &element.namespaces {
                    start = start.ns(prefix.as_str(), uri.as_str());
                }
                for (name, value) in &element.attributes {
                    start = start.attr(name.as_str(), value.as_str());
                }
                stack.push((None, None));
                for child in element.content.iter().rev() {
                    match child {
                        XmlContent::Element(element) => stack.push((Some(element), None)),
                        XmlContent::Text(text) => stack.push((None, Some(text.as_str()))),
                    }
                }
                start.into()
            }
            (None, Some(text)) => xml::writer::XmlEvent::characters(text),
            (None, None) => xml::writer::XmlEvent::end_element().into(),
        };
        writer
            .write(event)
            .map_err(|_| Error::new(Kind::Protocol))?;
        if writer.inner_ref().len() > XML_SIZE_BYTES_MAX {
            return Err(Error::new(Kind::Limit));
        }
    }
    Ok(bytes)
}
