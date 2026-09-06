use super::{Query, RuleKind, nonzero};
use crate::document::Node;
use crate::error::Kind;
use crate::{Document, Error};

/// A typed capability resource; channel values retain their endpoint-specific meaning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Endpoint {
    /// General device capabilities.
    Device,
    /// Event generation capabilities.
    Events,
    /// Motion configuration for a video input channel.
    Motion(u32),
    /// Encoding capabilities for a stream ID such as 101.
    Stream(u32),
    /// One smart-event configuration family and channel.
    Rule(RuleKind, u32),
    /// PTZ capabilities for a channel.
    Ptz(u32),
    /// Two-way audio codec options and bounds for an audio channel.
    Audio(u32),
    /// Callback host count, authentication methods and field bounds.
    CallbackHosts,
}

impl Endpoint {
    fn resource(self) -> Result<String, Error> {
        Ok(match self {
            Self::Device => "/ISAPI/System/capabilities".to_owned(),
            Self::Events => "/ISAPI/Event/capabilities".to_owned(),
            Self::Motion(channel) => format!(
                "/ISAPI/System/Video/inputs/channels/{}/motionDetection/capabilities",
                nonzero(channel)?
            ),
            Self::Stream(channel) => format!(
                "/ISAPI/Streaming/channels/{}/capabilities",
                nonzero(channel)?
            ),
            Self::Rule(kind, channel) => format!(
                "/ISAPI/Smart/{}/{}/capabilities",
                kind.resource(),
                nonzero(channel)?
            ),
            Self::Ptz(channel) => {
                format!("/ISAPI/PTZCtrl/channels/{}/capabilities", nonzero(channel)?)
            }
            Self::Audio(channel) => format!(
                "/ISAPI/System/TwoWayAudio/channels/{}/capabilities",
                nonzero(channel)?
            ),
            Self::CallbackHosts => "/ISAPI/Event/notification/httpHosts/capabilities".to_owned(),
        })
    }
}

/// Reported capabilities with exact-path access to options and bounds.
#[derive(Clone, Debug)]
pub struct Capabilities {
    document: Document,
}

/// A reported field value, allowed options and inclusive numeric bounds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldCapability {
    value: Option<String>,
    options: Vec<String>,
    minimum: Option<i64>,
    maximum: Option<i64>,
}

impl FieldCapability {
    /// Returns a scalar capability value, when present.
    pub fn value(&self) -> Option<&str> {
        self.value.as_deref()
    }
    /// Returns the explicit advertised options, without adding defaults.
    pub fn options(&self) -> &[String] {
        &self.options
    }
    /// Returns the advertised inclusive minimum.
    pub const fn minimum(&self) -> Option<i64> {
        self.minimum
    }
    /// Returns the advertised inclusive maximum.
    pub const fn maximum(&self) -> Option<i64> {
        self.maximum
    }
}

impl Capabilities {
    /// Queries only the selected capability endpoint.
    ///
    /// # Errors
    /// Rejects invalid endpoint channel IDs.
    pub fn query(endpoint: Endpoint) -> Result<Query<Self>, Error> {
        Query::new(endpoint.resource()?, Self::parse)
    }
    fn parse(document: Document) -> Result<Self, Error> {
        if let Some(root) = document.xml_root() {
            if !crate::event::known_namespace(root.namespace()) {
                return Err(Error::new(Kind::Protocol));
            }
        } else if !document
            .json_value()
            .is_some_and(serde_json::Value::is_object)
        {
            return Err(Error::new(Kind::Protocol));
        }
        Ok(Self { document })
    }
    /// Reads a capability at an exact path relative to the response root.
    ///
    /// # Errors
    /// Rejects ambiguous paths, malformed bounds or more than 128 options.
    pub fn field(&self, path: &[&str]) -> Result<Option<FieldCapability>, Error> {
        if path.len() > 16 {
            return Err(Error::new(Kind::Limit));
        }
        let mut node = self.document.node();
        for name in path {
            let Some(child) = node.child(name)? else {
                return Ok(None);
            };
            node = child;
        }
        let (value, options, minimum, maximum) = match node {
            Node::Xml(element) => (
                Some(node.text()?).filter(|value| !value.is_empty()),
                element.attributes().get("opt").cloned(),
                element.attributes().get("min").cloned(),
                element.attributes().get("max").cloned(),
            ),
            Node::Json(value) if value.is_object() => (
                node.field("value")?,
                node.field("@opt")?,
                node.field("@min")?,
                node.field("@max")?,
            ),
            Node::Json(_) => (Some(node.text()?), None, None, None),
        };
        let parse = |value: Option<String>| -> Result<Option<i64>, Error> {
            value
                .filter(|value| !value.is_empty())
                .map(|value| value.parse().map_err(|_| Error::new(Kind::Protocol)))
                .transpose()
        };
        let options: Vec<_> = options.map_or_else(Vec::new, |options| {
            options
                .split(',')
                .map(|option| option.trim().to_owned())
                .collect()
        });
        let minimum = parse(minimum)?;
        let maximum = parse(maximum)?;
        if options.len() > 128
            || minimum
                .zip(maximum)
                .is_some_and(|(minimum, maximum)| minimum > maximum)
        {
            return Err(Error::new(Kind::Protocol));
        }
        Ok(Some(FieldCapability {
            value,
            options,
            minimum,
            maximum,
        }))
    }
    /// Returns unknown capabilities without interpreting their meaning.
    pub const fn document(&self) -> &Document {
        &self.document
    }
}
