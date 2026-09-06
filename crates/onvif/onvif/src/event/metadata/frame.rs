use chrono::{DateTime, NaiveDateTime, Utc};
use xmltree::Element;

use super::geometry::Transform;
use super::{
    Frame, Object, ProtocolError, SCHEMA, classification, geometry, recognition, tree, xml,
};

const FRAME_COUNT_MAX: usize = 64;
const OBJECT_COUNT_MAX: usize = 128;
const DELETE_COUNT_MAX: usize = 128;

pub(super) fn parse(root: &Element) -> Result<Vec<Frame>, ProtocolError> {
    let frames =
        tree::children(root, "VideoAnalytics").flat_map(|stream| tree::children(stream, "Frame"));
    tree::bounded(frames, FRAME_COUNT_MAX, parse_frame)
}

fn parse_frame(element: &Element) -> Result<Frame, ProtocolError> {
    let utc_time = parse_time(tree::attribute(element, "UtcTime")?)?;
    let source = element
        .attributes
        .get("Source")
        .map(|value| tree::string(value))
        .transpose()?;
    let transform = Transform::frame(element)?;
    let objects = tree::bounded(
        tree::children(element, "Object"),
        OBJECT_COUNT_MAX,
        |element| parse_object(element, &transform),
    )?;
    let deleted_ids = match xml::child(element, SCHEMA, "ObjectTree")? {
        Some(tree) => tree::bounded(
            tree::children(tree, "Delete"),
            DELETE_COUNT_MAX,
            parse_delete,
        )?,
        None => Vec::new(),
    };
    Ok(Frame {
        utc_time,
        source,
        objects,
        deleted_ids,
    })
}

fn parse_object(element: &Element, transform: &Transform) -> Result<Object, ProtocolError> {
    let mut object = Object {
        id: parse_id(element)?,
        class: None,
        confidence: None,
        bbox: None,
        text: None,
    };
    if let Some(appearance) = xml::child(element, SCHEMA, "Appearance")? {
        object.bbox = geometry::parse(appearance, transform)?;
        let classification = classification::parse(appearance)?;
        object.class = classification.class;
        object.confidence = classification.confidence;
        object.text = recognition::parse(appearance)?;
    }
    Ok(object)
}

fn parse_id(element: &Element) -> Result<String, ProtocolError> {
    tree::string(tree::attribute(element, "ObjectId")?)
}

fn parse_delete(element: &Element) -> Result<String, ProtocolError> {
    if !xml::text(element)?.is_empty() {
        return Err(ProtocolError("unexpected metadata deletion content"));
    }
    parse_id(element)
}

fn parse_time(value: &str) -> Result<DateTime<Utc>, ProtocolError> {
    let value = value.trim();
    DateTime::parse_from_rfc3339(value)
        .map(|time| time.to_utc())
        .or_else(|_| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f").map(|time| time.and_utc())
        })
        .map_err(|_| ProtocolError("invalid metadata frame time"))
}
