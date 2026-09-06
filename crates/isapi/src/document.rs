use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;
use serde_json::Value;
use xml::reader::{ParserConfig, XmlEvent};

use crate::Error;

mod write;
use crate::error::Kind;

/// A bounded, namespace-aware XML or JSON document with unknown fields retained.
#[derive(Clone, Eq, PartialEq, Serialize)]
pub struct Document {
    content: Content,
}

#[derive(Clone, Eq, PartialEq, Serialize)]
enum Content {
    Xml(Element),
    Json(Value),
}

/// An XML element with ordered content, attributes and local namespace declarations.
#[derive(Clone, Eq, PartialEq, Serialize)]
pub struct Element {
    name: String,
    namespace: Option<String>,
    namespaces: BTreeMap<String, String>,
    attributes: BTreeMap<String, String>,
    content: Vec<XmlContent>,
}

#[derive(Clone, Eq, PartialEq, Serialize)]
enum XmlContent {
    Text(String),
    Element(Element),
}

impl Document {
    /// Parses a bounded structured HTTP body using its declared media type and charset.
    ///
    /// # Errors
    /// Rejects unsupported media types, duplicate charsets and parser failures.
    pub fn parse(content_type: &str, bytes: &[u8]) -> Result<Self, Error> {
        if content_type.len() > 8192 {
            return Err(Error::new(Kind::Limit));
        }
        let media: mime::Mime = content_type
            .parse()
            .map_err(|_| Error::new(Kind::Protocol))?;
        let charsets: Vec<_> = media
            .params()
            .filter(|(name, _)| name == &mime::CHARSET)
            .collect();
        if charsets.len() > 1 {
            return Err(Error::new(Kind::Protocol));
        }
        let charset = charsets.first().map(|(_, value)| value.as_str());
        match media.essence_str() {
            "application/xml" | "text/xml" => Self::parse_xml(bytes, charset),
            "application/json" => Self::parse_json(bytes, charset),
            _ => Err(Error::new(Kind::Protocol)),
        }
    }

    pub(crate) fn root(&self, name: &str) -> Result<Node<'_>, Error> {
        match &self.content {
            Content::Xml(root)
                if root.name.rsplit(':').next() == Some(name)
                    && crate::event::known_namespace(root.namespace.as_deref()) =>
            {
                Ok(Node::Xml(root))
            }
            Content::Json(value) if value.is_object() => {
                Ok(Node::Json(value.get(name).unwrap_or(value)))
            }
            _ => Err(Error::new(Kind::Protocol)),
        }
    }
    /// Parses XML with strict charset reconciliation and bounded structural complexity.
    ///
    /// # Errors
    /// Rejects malformed XML, DTDs, encoding conflicts and resource-limit violations.
    pub fn parse_xml(bytes: impl AsRef<[u8]>, charset: Option<&str>) -> Result<Self, Error> {
        let text = crate::encoding::xml(bytes.as_ref(), charset)?;
        let parser = ParserConfig::new()
            .whitespace_to_characters(true)
            .cdata_to_characters(true)
            .ignore_comments(true)
            .coalesce_characters(true)
            .allow_multiple_root_elements(false)
            .ignore_invalid_encoding_declarations(false)
            .replace_unknown_entity_references(false)
            .max_entity_expansion_length(4096)
            .max_entity_expansion_depth(1)
            .max_name_length(256)
            .max_attributes(32)
            .max_attribute_length(4096)
            .max_data_length(64 * 1024)
            .create_reader(text.as_bytes());
        let mut builder = XmlBuilder::default();
        for event in parser {
            builder.accept(event?)?;
        }
        let root = builder.root.ok_or_else(|| Error::new(Kind::Protocol))?;
        if !builder.stack.is_empty() {
            return Err(Error::new(Kind::Protocol));
        }
        Ok(Self {
            content: Content::Xml(root),
        })
    }

    /// Parses JSON without duplicate keys or silent character replacement.
    ///
    /// # Errors
    /// Rejects malformed input, duplicate keys, charset conflicts and excessive structure.
    pub fn parse_json(bytes: impl AsRef<[u8]>, charset: Option<&str>) -> Result<Self, Error> {
        let text = crate::encoding::json(bytes.as_ref(), charset)?;
        crate::event::json::validate(text.as_bytes())?;
        Ok(Self {
            content: Content::Json(serde_json::from_str(&text)?),
        })
    }

    /// Returns the XML root when this is an XML document.
    pub const fn xml_root(&self) -> Option<&Element> {
        match &self.content {
            Content::Xml(root) => Some(root),
            Content::Json(_) => None,
        }
    }

    /// Returns the JSON value when this is a JSON document.
    pub const fn json_value(&self) -> Option<&Value> {
        match &self.content {
            Content::Json(value) => Some(value),
            Content::Xml(_) => None,
        }
    }

    pub(crate) const fn node(&self) -> Node<'_> {
        match &self.content {
            Content::Xml(root) => Node::Xml(root),
            Content::Json(value) => Node::Json(value),
        }
    }
}

impl Element {
    /// Returns the qualified XML name, including its prefix when present.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Returns the resolved namespace URI.
    pub fn namespace(&self) -> Option<&str> {
        self.namespace.as_deref()
    }
    /// Returns attributes by their qualified names.
    pub const fn attributes(&self) -> &BTreeMap<String, String> {
        &self.attributes
    }
    /// Iterates over direct child elements in document order.
    pub fn children(&self) -> impl Iterator<Item = &Self> {
        self.content.iter().filter_map(|item| match item {
            XmlContent::Element(child) => Some(child),
            XmlContent::Text(_) => None,
        })
    }

    fn scalar(&self) -> Result<String, Error> {
        let mut text = String::new();
        for item in &self.content {
            let XmlContent::Text(value) = item else {
                return Err(Error::new(Kind::Protocol));
            };
            if value.len() > 4096_usize.saturating_sub(text.len()) {
                return Err(Error::new(Kind::Limit));
            }
            text.push_str(value);
        }
        Ok(text.trim().to_owned())
    }
}

impl fmt::Debug for Document {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Document").finish_non_exhaustive()
    }
}
impl fmt::Debug for Element {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Element")
            .field("child_count", &self.content.len())
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct XmlBuilder {
    stack: Vec<Element>,
    root: Option<Element>,
    elements: usize,
}

impl XmlBuilder {
    fn accept(&mut self, event: XmlEvent) -> Result<(), Error> {
        match event {
            XmlEvent::Doctype { .. } => return Err(Error::new(Kind::Protocol)),
            XmlEvent::StartElement {
                name,
                attributes,
                namespace,
            } => {
                self.elements += 1;
                if self.elements > 8192 || self.stack.len() >= 32 || namespace.0.len() > 32 {
                    return Err(Error::new(Kind::Limit));
                }
                let namespaces = namespace
                    .0
                    .into_iter()
                    .filter(|(prefix, uri)| {
                        *prefix != "xml"
                            && self
                                .stack
                                .iter()
                                .rev()
                                .find_map(|parent| parent.namespaces.get(prefix))
                                != Some(uri)
                    })
                    .collect();
                let attributes = attributes
                    .into_iter()
                    .map(|attribute| (qualified(&attribute.name), attribute.value))
                    .collect();
                self.stack.push(Element {
                    name: qualified(&name),
                    namespace: name.namespace,
                    namespaces,
                    attributes,
                    content: Vec::new(),
                });
            }
            XmlEvent::EndElement { .. } => {
                let element = self.stack.pop().ok_or_else(|| Error::new(Kind::Protocol))?;
                if let Some(parent) = self.stack.last_mut() {
                    parent.content.push(XmlContent::Element(element));
                } else if self.root.replace(element).is_some() {
                    return Err(Error::new(Kind::Protocol));
                }
            }
            XmlEvent::Characters(text) | XmlEvent::CData(text) | XmlEvent::Whitespace(text) => {
                if let Some(parent) = self.stack.last_mut() {
                    parent.content.push(XmlContent::Text(text));
                } else if !text.trim().is_empty() {
                    return Err(Error::new(Kind::Protocol));
                }
            }
            _ => {}
        }
        Ok(())
    }
}

fn qualified(name: &xml::name::OwnedName) -> String {
    name.prefix.as_ref().map_or_else(
        || name.local_name.clone(),
        |prefix| format!("{prefix}:{}", name.local_name),
    )
}

#[derive(Clone, Copy)]
pub enum Node<'input> {
    Xml(&'input Element),
    Json(&'input Value),
}

impl fmt::Debug for Node<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Node").finish_non_exhaustive()
    }
}

impl<'input> Node<'input> {
    pub fn document(self) -> Document {
        match self {
            Self::Xml(element) => {
                let mut root = element.clone();
                if let Some(namespace) = &root.namespace {
                    let prefix = root
                        .name
                        .rsplit_once(':')
                        .map_or("", |(prefix, _)| prefix)
                        .to_owned();
                    root.namespaces.insert(prefix, namespace.clone());
                }
                Document {
                    content: Content::Xml(root),
                }
            }
            Self::Json(value) => Document {
                content: Content::Json(value.clone()),
            },
        }
    }
    pub fn children(self, name: &str) -> Vec<Self> {
        match self {
            Self::Xml(parent) => parent
                .children()
                .filter(|child| {
                    child.namespace == parent.namespace
                        && child.name.rsplit(':').next() == Some(name)
                })
                .map(Self::Xml)
                .collect(),
            Self::Json(value) => match value.get(name) {
                Some(Value::Array(values)) => values.iter().map(Self::Json).collect(),
                Some(value) => vec![Self::Json(value)],
                None => Vec::new(),
            },
        }
    }

    pub fn child(self, name: &str) -> Result<Option<Self>, Error> {
        let children = self.children(name);
        if children.len() > 1 {
            return Err(Error::new(Kind::Protocol));
        }
        Ok(children.first().copied())
    }

    pub fn text(self) -> Result<String, Error> {
        let text = match self {
            Self::Xml(element) => element.scalar()?,
            Self::Json(Value::String(value)) => value.trim().to_owned(),
            Self::Json(Value::Number(value)) => value.to_string(),
            Self::Json(Value::Bool(value)) => value.to_string(),
            _ => return Err(Error::new(Kind::Protocol)),
        };
        if text.len() > 4096 {
            return Err(Error::new(Kind::Limit));
        }
        Ok(text)
    }

    pub fn field(self, name: &str) -> Result<Option<String>, Error> {
        self.child(name)?.map(Self::text).transpose()
    }

    pub fn list(self, container: &str, item: &str) -> Result<Vec<Self>, Error> {
        match self {
            Self::Json(value) if value.get(container).is_some_and(Value::is_array) => {
                Ok(self.children(container))
            }
            _ => Ok(self
                .child(container)?
                .map_or_else(Vec::new, |node| node.children(item))),
        }
    }

    pub fn scalars(self) -> Result<BTreeMap<String, String>, Error> {
        let mut result = BTreeMap::new();
        match self {
            Self::Xml(element) => {
                for child in element
                    .children()
                    .filter(|child| child.namespace == element.namespace)
                {
                    if child.children().next().is_none()
                        && result
                            .insert(
                                child
                                    .name
                                    .rsplit(':')
                                    .next()
                                    .unwrap_or(&child.name)
                                    .to_owned(),
                                child.scalar()?,
                            )
                            .is_some()
                    {
                        return Err(Error::new(Kind::Protocol));
                    }
                }
            }
            Self::Json(Value::Object(values)) => {
                for (key, value) in values {
                    if !value.is_array() && !value.is_object() && !value.is_null() {
                        result.insert(key.clone(), Self::Json(value).text()?);
                    }
                }
            }
            _ => return Err(Error::new(Kind::Protocol)),
        }
        Ok(result)
    }
}
