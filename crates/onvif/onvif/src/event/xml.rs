use std::{
    borrow::Cow,
    io::{self, Write},
};

use ::xml::{
    attribute::{Attribute, OwnedAttribute},
    name::{Name, OwnedName},
    reader::{ParserConfig, XmlEvent},
    writer::{EventWriter, XmlEvent as WriteEvent},
};
use xmltree::{Element, Namespace, XMLNode};

use super::protocol::ProtocolError;

pub(super) const SOAP: &str = "http://www.w3.org/2003/05/soap-envelope";
pub(super) const ADDRESS: &str = "http://www.w3.org/2005/08/addressing";
pub(super) const EVENTS: &str = "http://www.onvif.org/ver10/events/wsdl";
pub(super) const NOTIFY: &str = "http://docs.oasis-open.org/wsn/b-2";

const NODE_COUNT_MAX: usize = 8192;
/// Bound aggregate reader-produced namespace storage, including inherited mappings and expanded names.
/// Charge copied strings cumulatively and mapping slots while they remain in the tree.
const NAMESPACE_SIZE_BYTES_MAX: usize = 2 * 1024 * 1024;
const NAME_SIZE_BYTES_MAX: usize = 256;
const ATTRIBUTE_SIZE_BYTES_MAX: usize = 4096;

pub(super) fn parse(bytes: &[u8], limit: usize) -> Result<Element, ProtocolError> {
    if bytes.len() > limit {
        return Err(ProtocolError("XML byte limit exceeded"));
    }
    let mut tree = Tree {
        pending: Vec::with_capacity(super::XML_DEPTH_MAX),
        root: None,
        element_count: 0,
        content_count: 0,
        namespace_bytes: 0,
        namespace_entries: 0,
    };
    let parser = ParserConfig::new()
        .trim_whitespace(false)
        .whitespace_to_characters(true)
        .cdata_to_characters(false)
        .ignore_comments(false)
        .coalesce_characters(true)
        .max_name_length(NAME_SIZE_BYTES_MAX)
        .max_attributes(32)
        .max_attribute_length(ATTRIBUTE_SIZE_BYTES_MAX)
        .max_data_length(64 * 1024)
        .max_entity_expansion_depth(1)
        .max_entity_expansion_length(4096)
        .create_reader(bytes);
    for event in parser {
        match event.map_err(|_| ProtocolError("invalid XML"))? {
            XmlEvent::StartElement {
                name,
                attributes,
                namespace,
            } => tree.start(name, attributes, namespace)?,
            XmlEvent::EndElement { .. } => tree.end()?,
            XmlEvent::Characters(text) | XmlEvent::Whitespace(text) => {
                tree.node(XMLNode::Text(text))?;
            }
            XmlEvent::CData(text) => tree.node(XMLNode::CData(text))?,
            XmlEvent::Comment(text) => tree.node(XMLNode::Comment(text))?,
            XmlEvent::ProcessingInstruction { name, data } => {
                tree.node(XMLNode::ProcessingInstruction(name, data))?;
            }
            XmlEvent::Doctype { .. } => return Err(ProtocolError("document types are forbidden")),
            XmlEvent::StartDocument { .. } | XmlEvent::EndDocument => {}
        }
    }
    if !tree.pending.is_empty() {
        return Err(ProtocolError("incomplete XML tree"));
    }
    tree.root.ok_or(ProtocolError("missing XML root"))
}

struct Tree {
    pending: Vec<Element>,
    root: Option<Element>,
    element_count: usize,
    content_count: usize,
    namespace_bytes: usize,
    namespace_entries: usize,
}

impl Tree {
    fn start(
        &mut self,
        name: OwnedName,
        attributes: Vec<OwnedAttribute>,
        namespace: Namespace,
    ) -> Result<(), ProtocolError> {
        if self.pending.len() >= super::XML_DEPTH_MAX {
            return Err(ProtocolError("XML structure limit exceeded"));
        }
        count_node(&mut self.element_count)?;
        super::validate_unique_attributes(&attributes)
            .map_err(|_| ProtocolError("duplicate XML attributes"))?;
        let namespace_bytes =
            self.namespace_bytes
                .saturating_add(namespace_size(&name, &attributes, &namespace)?);
        let namespace_entries = self.namespace_entries.saturating_add(namespace.0.len());
        let storage_bytes = namespace_entries
            .saturating_mul(size_of::<(String, String)>())
            .saturating_add(namespace_bytes);
        if storage_bytes > NAMESPACE_SIZE_BYTES_MAX {
            return Err(ProtocolError("XML namespace storage limit exceeded"));
        }
        self.namespace_bytes = namespace_bytes;
        self.namespace_entries = namespace_entries;
        self.pending.push(Element {
            name: name.local_name,
            prefix: name.prefix,
            namespace: name.namespace,
            namespaces: Some(namespace),
            attributes: attributes
                .into_iter()
                .map(|attribute| {
                    let key = match attribute.name.prefix {
                        Some(prefix) => format!("{prefix}:{}", attribute.name.local_name),
                        None => attribute.name.local_name,
                    };
                    (key, attribute.value)
                })
                .collect(),
            children: Vec::new(),
        });
        Ok(())
    }

    fn end(&mut self) -> Result<(), ProtocolError> {
        let mut element = self
            .pending
            .pop()
            .ok_or(ProtocolError("invalid XML depth"))?;
        if let Some(parent) = self.pending.last_mut() {
            let removed = retain_scope(&mut element, parent);
            self.namespace_entries = self
                .namespace_entries
                .checked_sub(removed)
                .expect("removed namespace entries were already accounted");
            parent.children.push(XMLNode::Element(element));
        } else if self.root.replace(element).is_some() {
            return Err(ProtocolError("multiple XML roots"));
        }
        Ok(())
    }

    fn node(&mut self, node: XMLNode) -> Result<(), ProtocolError> {
        if let Some(parent) = self.pending.last_mut() {
            count_node(&mut self.content_count)?;
            parent.children.push(node);
        }
        Ok(())
    }
}

const fn count_node(count: &mut usize) -> Result<(), ProtocolError> {
    if *count >= NODE_COUNT_MAX {
        return Err(ProtocolError("XML structure limit exceeded"));
    }
    *count += 1;
    Ok(())
}

fn namespace_size(
    name: &OwnedName,
    attributes: &[OwnedAttribute],
    namespace: &Namespace,
) -> Result<usize, ProtocolError> {
    if namespace.iter().any(|(prefix, uri)| {
        prefix.len() > NAME_SIZE_BYTES_MAX || uri.len() > ATTRIBUTE_SIZE_BYTES_MAX
    }) {
        return Err(ProtocolError("XML namespace declaration limit exceeded"));
    }
    let mappings = namespace
        .iter()
        .map(|(prefix, uri)| prefix.len() + uri.len());
    let names = std::iter::once(name).chain(attributes.iter().map(|attribute| &attribute.name));
    let sizes = mappings.chain(names.map(|name| {
        name.namespace.as_ref().map_or(0, String::len) + name.prefix.as_ref().map_or(0, String::len)
    }));
    Ok(sizes.fold(0, usize::saturating_add))
}

fn retain_scope(element: &mut Element, parent: &Element) -> usize {
    if matches(element, NOTIFY, "NotificationMessage")
        || matches(element, ADDRESS, "ReferenceParameters")
        || matches(parent, ADDRESS, "ReferenceParameters")
    {
        return 0;
    }
    if let Some(namespace) = element.namespaces.as_mut() {
        let before = namespace.0.len();
        namespace.0.retain(|prefix, uri| {
            parent
                .namespaces
                .as_ref()
                .and_then(|scope| scope.get(prefix))
                != Some(uri.as_str())
        });
        before - namespace.0.len()
    } else {
        0
    }
}

pub(super) fn payload(root: &Element) -> Result<&Element, ProtocolError> {
    if matches(root, SOAP, "Envelope") {
        let body = required(root, SOAP, "Body")?;
        let mut children = body.children.iter().filter_map(XMLNode::as_element);
        let payload = children.next().ok_or(ProtocolError("empty SOAP body"))?;
        if children.next().is_some() {
            return Err(ProtocolError("ambiguous SOAP body"));
        }
        Ok(payload)
    } else {
        Ok(root)
    }
}

pub(super) fn matches(element: &Element, namespace: &str, name: &str) -> bool {
    element.name == name && element.namespace.as_deref() == Some(namespace)
}

pub(super) fn child<'a>(
    parent: &'a Element,
    namespace: &str,
    name: &str,
) -> Result<Option<&'a Element>, ProtocolError> {
    let mut matches = parent
        .children
        .iter()
        .filter_map(XMLNode::as_element)
        .filter(|element| matches(element, namespace, name));
    let first = matches.next();
    if matches.next().is_some() {
        return Err(ProtocolError("duplicate XML field"));
    }
    Ok(first)
}

pub(super) fn required<'a>(
    parent: &'a Element,
    namespace: &str,
    name: &str,
) -> Result<&'a Element, ProtocolError> {
    child(parent, namespace, name)?.ok_or(ProtocolError("missing XML field"))
}

pub(super) fn text(element: &Element) -> Result<String, ProtocolError> {
    if element
        .children
        .iter()
        .any(|node| node.as_element().is_some())
    {
        return Err(ProtocolError("expected XML scalar"));
    }
    let text = element.get_text().unwrap_or_default().trim().to_owned();
    if text.len() > 4096 {
        return Err(ProtocolError("XML scalar limit exceeded"));
    }
    Ok(text)
}

pub(super) fn field(
    parent: &Element,
    namespace: &str,
    name: &str,
) -> Result<String, ProtocolError> {
    text(required(parent, namespace, name)?)
}

pub(super) fn element(namespace: &str, prefix: &str, name: &str, text: Option<&str>) -> Element {
    let mut node = Element::new(name);
    node.prefix = Some(prefix.to_owned());
    node.namespace = Some(namespace.to_owned());
    let mut namespaces = Namespace::empty();
    namespaces.put(prefix, namespace);
    node.namespaces = Some(namespaces);
    if let Some(text) = text {
        node.children.push(XMLNode::Text(text.to_owned()));
    }
    node
}

pub(super) fn encode(element: &Element) -> Result<String, ProtocolError> {
    let config = xmltree::EmitterConfig {
        write_document_declaration: false,
        perform_indent: false,
        perform_escaping: true,
        autopad_comments: false,
        cdata_to_characters: false,
        ..xmltree::EmitterConfig::new()
    };
    let mut writer = EventWriter::new_with_config(Output::default(), config);
    let mut scopes = Vec::with_capacity(super::XML_DEPTH_MAX);
    let result = write_element(&mut writer, element, &mut scopes);
    if writer.inner_ref().overflowed {
        return Err(ProtocolError("serialized XML exceeds byte limit"));
    }
    result?;
    String::from_utf8(writer.into_inner().bytes)
        .map_err(|_| ProtocolError("invalid serialized XML"))
}

fn write_element<'tree>(
    writer: &mut EventWriter<Output>,
    element: &'tree Element,
    scopes: &mut Vec<Option<&'tree Namespace>>,
) -> Result<(), ProtocolError> {
    if scopes.len() >= super::XML_DEPTH_MAX {
        return Err(ProtocolError("XML structure limit exceeded"));
    }
    write_start(writer, element, scopes)?;
    scopes.push(element.namespaces.as_ref());
    for child in &element.children {
        let event = match child {
            XMLNode::Element(child) => {
                write_element(writer, child, scopes)?;
                continue;
            }
            XMLNode::Text(text) => WriteEvent::Characters(text),
            XMLNode::CData(text) => WriteEvent::CData(text),
            XMLNode::Comment(text) => WriteEvent::Comment(text),
            XMLNode::ProcessingInstruction(name, data) => WriteEvent::ProcessingInstruction {
                name,
                data: data.as_deref(),
            },
        };
        writer
            .write(event)
            .map_err(|_| ProtocolError("XML serialization failed"))?;
    }
    assert!(
        scopes.pop().is_some(),
        "serialized element has an active namespace scope"
    );
    writer
        .write(WriteEvent::EndElement {
            name: Some(xml_name(element)),
        })
        .map_err(|_| ProtocolError("XML serialization failed"))
}

/// Emit explicit declarations because writer namespace deduplication skips resets and shadowed bindings.
fn write_start(
    writer: &mut EventWriter<Output>,
    element: &Element,
    scopes: &[Option<&Namespace>],
) -> Result<(), ProtocolError> {
    let attributes = element.attributes.iter().map(|(key, value)| Attribute {
        name: Name::local(key),
        value,
    });
    let declarations = element
        .namespaces
        .iter()
        .flat_map(Namespace::iter)
        .filter(|(prefix, uri)| {
            !matches!(*prefix, "xml" | "xmlns")
                && scopes
                    .iter()
                    .rev()
                    .flatten()
                    .find_map(|scope| scope.get(prefix))
                    .unwrap_or("")
                    != *uri
        })
        .map(|(prefix, uri)| Attribute {
            name: Name {
                local_name: if prefix.is_empty() { "xmlns" } else { prefix },
                namespace: None,
                prefix: if prefix.is_empty() {
                    None
                } else {
                    Some("xmlns")
                },
            },
            value: uri,
        });
    writer
        .write(WriteEvent::StartElement {
            name: xml_name(element),
            attributes: Cow::Owned(attributes.chain(declarations).collect()),
            namespace: Cow::Owned(Namespace::empty()),
        })
        .map_err(|_| ProtocolError("XML serialization failed"))
}

fn xml_name(element: &Element) -> Name<'_> {
    Name {
        local_name: &element.name,
        namespace: element.namespace.as_deref(),
        prefix: element.prefix.as_deref(),
    }
}

#[derive(Default)]
struct Output {
    bytes: Vec<u8>,
    overflowed: bool,
}

impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > super::NOTIFICATION_XML_SIZE_BYTES_MAX - self.bytes.len() {
            self.overflowed = true;
            return Err(io::Error::other("serialized XML exceeds byte limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
