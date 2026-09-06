use xmltree::Element;

use super::{BoundingBox, ProtocolError, SCHEMA, tree, xml};

#[derive(Clone, Copy)]
pub(super) struct Transform {
    translate: [f64; 2],
    scale: [f64; 2],
}

impl Transform {
    pub(super) fn frame(element: &Element) -> Result<Self, ProtocolError> {
        Self {
            translate: [0.0; 2],
            scale: [1.0; 2],
        }
        .child(element)
    }

    fn child(&self, parent: &Element) -> Result<Self, ProtocolError> {
        let Some(element) = xml::child(parent, SCHEMA, "Transformation")? else {
            return Ok(*self);
        };
        let translate = vector(element, "Translate", [0.0; 2])?;
        let scale = vector(element, "Scale", [1.0; 2])?;
        let combined = Self {
            translate: [
                self.scale[0].mul_add(translate[0], self.translate[0]),
                self.scale[1].mul_add(translate[1], self.translate[1]),
            ],
            scale: [self.scale[0] * scale[0], self.scale[1] * scale[1]],
        };
        if combined.translate.iter().any(|value| !value.is_finite()) {
            return Err(ProtocolError("invalid metadata translation"));
        }
        if combined
            .scale
            .iter()
            .any(|value| !value.is_finite() || *value == 0.0)
        {
            return Err(ProtocolError("invalid metadata scale"));
        }
        Ok(combined)
    }
}

pub(super) fn parse(
    appearance: &Element,
    frame: &Transform,
) -> Result<Option<BoundingBox>, ProtocolError> {
    let transform = frame.child(appearance)?;
    let Some(shape) = xml::child(appearance, SCHEMA, "Shape")? else {
        return Ok(None);
    };
    let element = xml::required(shape, SCHEMA, "BoundingBox")?;
    empty(element)?;
    let edges = [
        transform.scale[0].mul_add(number(element, "left")?, transform.translate[0]),
        transform.scale[0].mul_add(number(element, "right")?, transform.translate[0]),
        transform.scale[1].mul_add(number(element, "top")?, transform.translate[1]),
        transform.scale[1].mul_add(number(element, "bottom")?, transform.translate[1]),
    ];
    Ok(Some(normalize(edges)?))
}

fn normalize(edges: [f64; 4]) -> Result<BoundingBox, ProtocolError> {
    if edges
        .iter()
        .any(|value| !value.is_finite() || !(-1.0..=1.0).contains(value))
    {
        return Err(ProtocolError("metadata box outside normalized image"));
    }
    let [left, right, top, bottom] = edges;
    if left >= right || bottom >= top {
        return Err(ProtocolError("invalid metadata box edges"));
    }
    let bbox = BoundingBox {
        x: ((left + 1.0) / 2.0) as f32,
        y: ((1.0 - top) / 2.0) as f32,
        width: ((right - left) / 2.0) as f32,
        height: ((top - bottom) / 2.0) as f32,
    };
    if bbox.width <= 0.0 || bbox.height <= 0.0 {
        return Err(ProtocolError("metadata box loses precision"));
    }
    if bbox.x + bbox.width > 1.0 || bbox.y + bbox.height > 1.0 {
        return Err(ProtocolError("metadata box loses precision"));
    }
    Ok(bbox)
}

fn vector(parent: &Element, name: &str, default: [f64; 2]) -> Result<[f64; 2], ProtocolError> {
    let Some(element) = xml::child(parent, SCHEMA, name)? else {
        return Ok(default);
    };
    empty(element)?;
    Ok([number(element, "x")?, number(element, "y")?])
}

fn number(element: &Element, name: &str) -> Result<f64, ProtocolError> {
    let value = tree::attribute(element, name)?
        .trim()
        .parse::<f64>()
        .map_err(|_| ProtocolError("invalid metadata coordinate"))?;
    if !value.is_finite() {
        return Err(ProtocolError("nonfinite metadata coordinate"));
    }
    Ok(value)
}

fn empty(element: &Element) -> Result<(), ProtocolError> {
    if !xml::text(element)?.is_empty() {
        return Err(ProtocolError("unexpected metadata geometry content"));
    }
    Ok(())
}
