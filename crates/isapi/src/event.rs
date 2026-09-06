use std::fmt;
use std::io::BufReader;

use xml::name::OwnedName;
use xml::reader::{ParserConfig, XmlEvent};

use crate::error::Kind;
use crate::{Document, Error, ImageRef, Object};

pub mod json;

const FIELD_SIZE_BYTES_MAX: usize = 4096;
const DEPTH_MAX: usize = 32;
const ELEMENT_COUNT_MAX: usize = 8192;
const FIELDS: [&str; 8] = [
    "eventType",
    "eventState",
    "channelID",
    "dynChannelID",
    "dateTime",
    "activePostCount",
    "detectionTarget",
    "channelName",
];

/// Explicit fields from a camera's EventNotificationAlert, without inferred classifications.
#[derive(Clone, Eq, PartialEq)]
pub struct Event {
    event_type: String,
    state: String,
    channel_id: Option<u32>,
    dynamic_channel_id: Option<u32>,
    date_time: Option<String>,
    active_post_count: Option<u32>,
    detection_target: Option<String>,
    channel_name: Option<String>,
    id: Option<String>,
    objects: Vec<Object>,
    images: Vec<ImageRef>,
    data: Document,
}

impl Event {
    /// Extracts bounded event XML without reading external entities or clock state.
    ///
    /// Only direct scalar children in the root's namespace are interpreted. Unknown
    /// extensions remain in [`Self::data`]; supported analytics have typed projections.
    ///
    /// # Errors
    /// Rejects invalid XML, unsupported encodings/namespaces, missing type/state,
    /// duplicate fields, invalid channel/count values, DTDs, and excessive sizes/depth.
    pub fn parse(xml: impl AsRef<[u8]>) -> Result<Self, Error> {
        Self::parse_xml_charset(xml.as_ref(), None)
    }

    pub(crate) fn parse_xml_charset(bytes: &[u8], charset: Option<&str>) -> Result<Self, Error> {
        let data = Document::parse_xml(bytes, charset)?;
        let xml = crate::encoding::xml(bytes, charset)?;
        let parser = ParserConfig::new()
            .whitespace_to_characters(true)
            .cdata_to_characters(true)
            .ignore_comments(true)
            .coalesce_characters(true)
            .allow_multiple_root_elements(false)
            .ignore_invalid_encoding_declarations(false)
            .replace_unknown_entity_references(false)
            .max_entity_expansion_length(FIELD_SIZE_BYTES_MAX)
            .max_entity_expansion_depth(1)
            .max_name_length(256)
            .max_attributes(32)
            .max_attribute_length(FIELD_SIZE_BYTES_MAX)
            .max_data_length(64 * 1024)
            .create_reader(BufReader::new(xml.as_bytes()));
        let mut builder = Builder::default();
        for event in parser {
            builder.accept(event?)?;
        }
        builder.finish(data)
    }

    /// Extracts the same explicit event fields from a JSON notification or notification wrapper.
    ///
    /// # Errors
    /// Rejects malformed, oversized, excessively nested JSON, duplicate fields, and invalid event fields.
    pub fn parse_json(bytes: impl AsRef<[u8]>) -> Result<Self, Error> {
        Self::parse_json_charset(bytes.as_ref(), None)
    }

    pub(crate) fn parse_json_charset(bytes: &[u8], charset: Option<&str>) -> Result<Self, Error> {
        let data = Document::parse_json(bytes, charset)?;
        let text = crate::encoding::json(bytes, charset)?;
        Builder {
            root_seen: true,
            fields: json::parse(text.as_bytes())?,
            ..Builder::default()
        }
        .finish(data)
    }

    /// Returns the camera's event UUID or explicit event ID, without inventing one.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }
    /// Returns bounded object observations from supported analytics structures.
    pub fn objects(&self) -> &[Object] {
        &self.objects
    }
    /// Returns explicit MIME image references, never remote URLs to fetch automatically.
    pub fn images(&self) -> &[ImageRef] {
        &self.images
    }
    /// Returns the validated document, including uninterpreted vendor extensions.
    pub const fn data(&self) -> &Document {
        &self.data
    }

    /// Identifies the two documented heartbeat encodings without treating them as alarms.
    pub fn is_heartbeat(&self) -> bool {
        (self.event_type.eq_ignore_ascii_case("videoloss") && self.state == "inactive")
            || (self.event_type.eq_ignore_ascii_case("heartBeat") && self.state == "active")
    }

    /// Returns the original event type, including unrecognized vendor values.
    pub fn event_type(&self) -> &str {
        &self.event_type
    }

    /// Returns the original event state, including unrecognized vendor values.
    pub fn state(&self) -> &str {
        &self.state
    }

    /// Maps only explicit active/inactive states; unknown values have no Boolean interpretation.
    pub fn active(&self) -> Option<bool> {
        match self.state.as_str() {
            "active" => Some(true),
            "inactive" => Some(false),
            _ => None,
        }
    }

    /// Identifies the camera's VMD motion event, not a person or vehicle detection.
    pub fn is_motion(&self) -> bool {
        self.event_type.eq_ignore_ascii_case("VMD")
    }

    /// Returns the explicit channelID without inferring an NVR channel mapping.
    pub const fn channel_id(&self) -> Option<u32> {
        self.channel_id
    }

    /// Returns dynChannelID separately from channelID when present.
    pub const fn dynamic_channel_id(&self) -> Option<u32> {
        self.dynamic_channel_id
    }

    /// Returns the original timestamp string without normalization or clock-skew correction.
    pub fn date_time(&self) -> Option<&str> {
        self.date_time.as_deref()
    }

    /// Returns the reported active-post count, which is not a count of distinct objects.
    pub const fn active_post_count(&self) -> Option<u32> {
        self.active_post_count
    }

    /// Returns an explicit detectionTarget field, never a value inferred from camera settings.
    pub fn detection_target(&self) -> Option<&str> {
        self.detection_target.as_deref()
    }

    /// Returns the decoded channel name when the notification includes it.
    pub fn channel_name(&self) -> Option<&str> {
        self.channel_name.as_deref()
    }
}

impl fmt::Debug for Event {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Event").finish_non_exhaustive()
    }
}

#[derive(Default)]
struct Builder {
    depth: usize,
    element_count: usize,
    root_seen: bool,
    namespace: Option<String>,
    current_field: Option<usize>,
    fields: [Option<String>; 8],
}

impl Builder {
    fn accept(&mut self, event: XmlEvent) -> Result<(), Error> {
        match event {
            XmlEvent::StartDocument { encoding, .. }
                if !encoding.eq_ignore_ascii_case("UTF-8")
                    && !encoding.eq_ignore_ascii_case("US-ASCII") =>
            {
                return Err(Error::new(Kind::Protocol));
            }
            XmlEvent::Doctype { .. } => return Err(Error::new(Kind::Protocol)),
            XmlEvent::StartElement { name, .. } => self.start(name)?,
            XmlEvent::EndElement { .. } => {
                if self.depth == 2 {
                    self.current_field = None;
                }
                self.depth = self
                    .depth
                    .checked_sub(1)
                    .ok_or_else(|| Error::new(Kind::Protocol))?;
            }
            XmlEvent::Characters(text) | XmlEvent::CData(text) => {
                if let Some(index) = self.current_field {
                    let value = self.fields[index]
                        .as_mut()
                        .expect("selected scalar field was initialized");
                    if text.len() > FIELD_SIZE_BYTES_MAX.saturating_sub(value.len()) {
                        return Err(Error::new(Kind::Limit));
                    }
                    value.push_str(&text);
                } else if self.depth <= 1 && !text.trim().is_empty() {
                    return Err(Error::new(Kind::Protocol));
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn start(&mut self, name: OwnedName) -> Result<(), Error> {
        self.depth += 1;
        self.element_count += 1;
        if self.depth > DEPTH_MAX || self.element_count > ELEMENT_COUNT_MAX {
            return Err(Error::new(Kind::Limit));
        }
        if self.depth == 1 {
            if self.root_seen
                || name.local_name != "EventNotificationAlert"
                || !known_namespace(name.namespace.as_deref())
            {
                return Err(Error::new(Kind::Protocol));
            }
            self.root_seen = true;
            self.namespace = name.namespace;
        } else if self.current_field.is_some() {
            return Err(Error::new(Kind::Protocol));
        } else if self.depth == 2
            && name.namespace == self.namespace
            && let Some(index) = FIELDS.iter().position(|field| *field == name.local_name)
        {
            if self.fields[index].is_some() {
                return Err(Error::new(Kind::Protocol));
            }
            self.fields[index] = Some(String::new());
            self.current_field = Some(index);
        }
        Ok(())
    }

    fn finish(self, data: Document) -> Result<Event, Error> {
        if !self.root_seen || self.depth != 0 {
            return Err(Error::new(Kind::Protocol));
        }
        if self
            .fields
            .iter()
            .flatten()
            .any(|value| value.len() > FIELD_SIZE_BYTES_MAX)
        {
            return Err(Error::new(Kind::Limit));
        }
        let [
            event_type,
            state,
            channel,
            dynamic_channel,
            date_time,
            count,
            target,
            name,
        ] = self
            .fields
            .map(|value| value.map(|value| value.trim().to_owned()));
        let event_type = required(event_type)?;
        let root = data.node();
        let root = if data.json_value().is_some() {
            root.child("EventNotificationAlert")?.unwrap_or(root)
        } else {
            root
        };
        let analytics = crate::analytics::parse(root, &event_type)?;
        Ok(Event {
            event_type,
            state: required(state)?,
            channel_id: optional_number(channel)?,
            dynamic_channel_id: optional_number(dynamic_channel)?,
            date_time,
            active_post_count: optional_number(count)?,
            detection_target: target,
            channel_name: name,
            id: analytics.id,
            objects: analytics.objects,
            images: analytics.images,
            data,
        })
    }
}

fn required(value: Option<String>) -> Result<String, Error> {
    value
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Error::new(Kind::Protocol))
}

fn optional_number(value: Option<String>) -> Result<Option<u32>, Error> {
    value
        .map(|value| value.parse().map_err(|_| Error::new(Kind::Protocol)))
        .transpose()
}

pub fn known_namespace(namespace: Option<&str>) -> bool {
    matches!(
        namespace,
        None | Some("http://www.hikvision.com/ver10/XMLSchema")
            | Some("http://www.hikvision.com/ver20/XMLSchema")
            | Some("http://www.isapi.org/ver20/XMLSchema")
            | Some("http://www.isapi.com/ver20/XMLSchema")
            | Some("http://www.std-cgi.com/ver20/XMLSchema")
            | Some("http://www.std-cgi.org/ver20/XMLSchema")
    )
}
