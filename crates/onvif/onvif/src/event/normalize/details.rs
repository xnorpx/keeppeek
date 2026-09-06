use crate::event::{BoundingBox, ItemList, ObjectClass, ProtocolError, XmlElement};

use super::classification;

const STRING_SIZE_BYTES_MAX: usize = 256;

pub(super) struct Details {
    pub(super) class: Option<ObjectClass>,
    pub(super) confidence: Option<f32>,
    pub(super) bbox: Option<BoundingBox>,
    pub(super) text: Option<String>,
    pub(super) count: Option<u64>,
}

pub(super) fn parse(data: &ItemList) -> Result<Details, ProtocolError> {
    let appearance = structured(data, "Object", &["Object"])
        .map(|object| child(object, "Appearance"))
        .transpose()?
        .flatten();
    let class = classification::parse(data, appearance)?;
    let mut details = Details {
        class: class.class,
        confidence: class.confidence,
        bbox: None,
        text: None,
        count: None,
    };
    for item in &data.simple {
        match item.name.as_str() {
            "Confidence" | "Likelihood" => {
                merge(&mut details.confidence, probability(&item.value)?)?;
            }
            "Count" | "ObjectCount" => merge(&mut details.count, count(&item.value)?)?,
            "Text" | "PlateNumber" | "LicensePlate" => {
                merge(&mut details.text, text(&item.value)?)?;
            }
            _ => {}
        }
    }
    if let Some(element) = structured(data, "BoundingBox", &["BoundingBox", "Rectangle"]) {
        merge(&mut details.bbox, rectangle(element)?)?;
    }
    if let Some(appearance) = appearance {
        if let Some(shape) = child(appearance, "Shape")?
            && let Some(element) = child(shape, "BoundingBox")?
        {
            if child(appearance, "Transformation")?.is_some() {
                return Err(ProtocolError(
                    "normalization box requires transformed coordinates",
                ));
            }
            merge(&mut details.bbox, rectangle(element)?)?;
        }
        if let Some(info) = child(appearance, "LicensePlateInfo")? {
            plate_text(info, &mut details.text)?;
        }
    }
    if let Some(info) = structured(data, "LicensePlateInfo", &["LicensePlateInfo"]) {
        plate_text(info, &mut details.text)?;
    }
    Ok(details)
}

fn count(value: &str) -> Result<u64, ProtocolError> {
    value
        .trim()
        .parse()
        .map_err(|_| ProtocolError("invalid normalization count"))
}

pub(super) fn probability(value: &str) -> Result<f32, ProtocolError> {
    let value: f64 = value
        .trim()
        .parse()
        .map_err(|_| ProtocolError("invalid normalization confidence"))?;
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(ProtocolError("invalid normalization confidence"));
    }
    Ok(value as f32)
}

fn text(value: &str) -> Result<String, ProtocolError> {
    if value.len() > STRING_SIZE_BYTES_MAX {
        return Err(ProtocolError("normalization text limit exceeded"));
    }
    Ok(value.to_owned())
}

fn plate_text(info: &XmlElement, target: &mut Option<String>) -> Result<(), ProtocolError> {
    if let Some(plate) = child(info, "PlateNumber")? {
        if let Some(value) = attribute(plate, "Likelihood") {
            probability(value)?;
        }
        merge(target, text(scalar(plate)?)?)?;
    }
    Ok(())
}

fn rectangle(element: &XmlElement) -> Result<BoundingBox, ProtocolError> {
    if !element.children.is_empty() || !element.text.trim().is_empty() {
        return Err(ProtocolError("invalid normalization rectangle"));
    }
    let [left, top, right, bottom] =
        ["left", "top", "right", "bottom"].map(|name| coordinate(element, name));
    let (left, top, right, bottom) = (left?, top?, right?, bottom?);
    if left >= right || bottom >= top {
        return Err(ProtocolError("empty normalization rectangle"));
    }
    let bbox = BoundingBox {
        x: ((left + 1.0) / 2.0) as f32,
        y: ((1.0 - top) / 2.0) as f32,
        width: ((right - left) / 2.0) as f32,
        height: ((top - bottom) / 2.0) as f32,
    };
    if bbox.width <= 0.0
        || bbox.height <= 0.0
        || bbox.x + bbox.width > 1.0
        || bbox.y + bbox.height > 1.0
        || bbox.x + bbox.width == bbox.x
        || bbox.y + bbox.height == bbox.y
    {
        return Err(ProtocolError("invalid normalization rectangle precision"));
    }
    Ok(bbox)
}

fn coordinate(element: &XmlElement, name: &str) -> Result<f64, ProtocolError> {
    let value: f64 = attribute(element, name)
        .ok_or(ProtocolError("missing normalization rectangle coordinate"))?
        .trim()
        .parse()
        .map_err(|_| ProtocolError("invalid normalization rectangle coordinate"))?;
    if !value.is_finite() || !(-1.0..=1.0).contains(&value) {
        return Err(ProtocolError(
            "normalization rectangle coordinate out of bounds",
        ));
    }
    Ok(value)
}

fn merge<Value: PartialEq>(target: &mut Option<Value>, value: Value) -> Result<(), ProtocolError> {
    if target.as_ref().is_some_and(|previous| previous != &value) {
        return Err(ProtocolError("contradictory normalization attributes"));
    }
    *target = Some(value);
    Ok(())
}

pub(super) fn structured<'a>(
    data: &'a ItemList,
    name: &str,
    roots: &[&str],
) -> Option<&'a XmlElement> {
    data.element
        .iter()
        .find(|item| item.name == name)
        .map(|item| &item.value)
        .filter(|element| roots.iter().any(|name| matches(element, name)))
}

pub(super) fn matches(element: &XmlElement, name: &str) -> bool {
    element.name.namespace_uri.as_deref() == Some(crate::event::ONVIF_SCHEMA_NAMESPACE)
        && element.name.local_name == name
}

pub(super) fn child<'a>(
    element: &'a XmlElement,
    name: &str,
) -> Result<Option<&'a XmlElement>, ProtocolError> {
    let mut children = element.children.iter().filter(|child| matches(child, name));
    let first = children.next();
    if children.next().is_some() {
        return Err(ProtocolError("duplicate normalization XML field"));
    }
    Ok(first)
}

pub(super) fn scalar(element: &XmlElement) -> Result<&str, ProtocolError> {
    if !element.children.is_empty() || element.text.len() > STRING_SIZE_BYTES_MAX {
        return Err(ProtocolError("invalid normalization XML scalar"));
    }
    Ok(element.text.trim())
}

pub(super) fn attribute<'a>(element: &'a XmlElement, name: &str) -> Option<&'a str> {
    element
        .attributes
        .iter()
        .find(|attribute| {
            attribute.name.local_name == name
                && attribute
                    .name
                    .namespace_uri
                    .as_deref()
                    .unwrap_or_default()
                    .is_empty()
        })
        .map(|attribute| attribute.value.as_str())
}
