use crate::Error;
use crate::document::Node;
use crate::error::Kind;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

const OBJECT_COUNT_MAX: usize = 64;
const IMAGE_COUNT_MAX: usize = 16;

/// A classification explicitly reported at a supported ISAPI field path.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum Target {
    /// A human target, not a motion-only inference.
    Person,
    /// A vehicle target, including an ANPR observation.
    Vehicle,
}

impl fmt::Display for Target {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Person => "person",
            Self::Vehicle => "vehicle",
        })
    }
}

/// A finite confidence in the inclusive interval from zero to one.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Confidence(f64);
impl Eq for Confidence {}

impl Confidence {
    /// Returns a unit-interval confidence, not a percentage.
    pub const fn normalized(self) -> f64 {
        self.0
    }
}

/// Coordinate units are preserved until image dimensions are available.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub enum CoordinateSpace {
    /// Absolute image pixel coordinates.
    Pixels,
    /// Coordinates in the inclusive interval from zero to one thousand.
    PerThousand,
    /// Coordinates in the inclusive interval from zero to one.
    Normalized,
}

/// A finite positive rectangle with its original coordinate space.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct BoundingBox {
    values: [f64; 4],
    space: CoordinateSpace,
}
impl Eq for BoundingBox {}

impl BoundingBox {
    /// Returns x, y, width and height in the declared coordinate space.
    pub const fn values(self) -> [f64; 4] {
        self.values
    }
    /// Returns the coordinate units; pixel rectangles need matching image dimensions.
    pub const fn space(self) -> CoordinateSpace {
        self.space
    }
    /// Normalizes against the associated image, never an unrelated video stream.
    ///
    /// # Errors
    /// Rejects missing pixel dimensions or rectangles outside the image.
    pub fn normalized(self, dimensions: Option<(u32, u32)>) -> Result<[f64; 4], Error> {
        let (width, height) = match self.space {
            CoordinateSpace::Pixels => dimensions
                .map(|(width, height)| (f64::from(width), f64::from(height)))
                .ok_or_else(|| Error::new(Kind::Protocol))?,
            CoordinateSpace::PerThousand => (1000.0, 1000.0),
            CoordinateSpace::Normalized => (1.0, 1.0),
        };
        let [left, top, box_width, box_height] = self.values;
        if width <= 0.0 || height <= 0.0 || left + box_width > width || top + box_height > height {
            return Err(Error::new(Kind::Protocol));
        }
        Ok([
            left / width,
            top / height,
            box_width / width,
            box_height / height,
        ])
    }
}

/// An explicit reference to one MIME image; the identifier is never a path to open.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ImageRef {
    id: String,
    kind: Option<String>,
}

impl ImageRef {
    /// Returns the opaque correlation identifier.
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Returns the camera's picture role when present.
    pub fn kind(&self) -> Option<&str> {
        self.kind.as_deref()
    }
}

/// One target observation with explicit identity, classification and image provenance.
#[derive(Clone, Eq, PartialEq, Serialize)]
pub struct Object {
    id: Option<String>,
    region: Option<String>,
    target: Option<Target>,
    confidence: Option<Confidence>,
    bbox: Option<BoundingBox>,
    image_id: Option<String>,
    plate: Option<String>,
    attributes: BTreeMap<String, String>,
}

impl Object {
    /// Returns the reported object identity, when the device supplies one.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }
    /// Returns the reported region identity without treating a region polygon as an object box.
    pub fn region(&self) -> Option<&str> {
        self.region.as_deref()
    }
    /// Returns a supported explicit classification.
    pub const fn target(&self) -> Option<Target> {
        self.target
    }
    /// Returns validated confidence with its scale normalized.
    pub const fn confidence(&self) -> Option<Confidence> {
        self.confidence
    }
    /// Returns the box in its original coordinate units.
    pub const fn bbox(&self) -> Option<BoundingBox> {
        self.bbox
    }
    /// Returns the image identifier to which this observation's box applies.
    pub fn image_id(&self) -> Option<&str> {
        self.image_id.as_deref()
    }
    /// Returns a license plate explicitly supplied by the camera.
    pub fn plate(&self) -> Option<&str> {
        self.plate.as_deref()
    }
    /// Returns scalar object attributes; nested unknown data remains in the event document.
    pub const fn attributes(&self) -> &BTreeMap<String, String> {
        &self.attributes
    }
}

impl fmt::Debug for Object {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Object")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
pub struct Analytics {
    pub id: Option<String>,
    pub objects: Vec<Object>,
    pub images: Vec<ImageRef>,
}

pub fn parse(root: Node<'_>, event_type: &str) -> Result<Analytics, Error> {
    let mut analytics = Analytics {
        id: alias(root, &["uuid", "eventID"])?,
        ..Analytics::default()
    };
    for region in root.list("DetectionRegionList", "DetectionRegionEntry")? {
        let region_id = region.field("regionID")?;
        let targets = region.list("TargetList", "Target")?;
        if targets.is_empty() {
            add_object(&mut analytics, region, region_id, None)?;
        } else {
            for target in targets {
                add_object(
                    &mut analytics,
                    target,
                    region_id.clone(),
                    classification(region)?,
                )?;
            }
        }
    }
    for target in root
        .list("TargetList", "Target")?
        .into_iter()
        .chain(root.children("detectionResult"))
    {
        add_object(&mut analytics, target, target.field("regionID")?, None)?;
    }
    if let Some(anpr) = root.child("ANPR")? {
        parse_anpr(&mut analytics, anpr)?;
    } else if analytics.objects.is_empty() {
        let inferred = match event_type.to_ascii_lowercase().as_str() {
            "humanrecognition" | "humandetection" => Some(Target::Person),
            "vehicledetection" => Some(Target::Vehicle),
            _ => None,
        };
        if inferred.is_some() || classification(root)?.is_some() {
            add_object(&mut analytics, root, root.field("regionID")?, inferred)?;
        }
    }
    for picture in root.list("pictureInfoList", "pictureInfo")? {
        image(&mut analytics.images, picture)?;
    }
    let mut identities = BTreeSet::new();
    for object in &analytics.objects {
        if let Some(id) = &object.id
            && !identities.insert((object.region.as_deref(), id))
        {
            return Err(Error::new(Kind::Protocol));
        }
        if let Some(id) = &object.image_id {
            add_image(&mut analytics.images, id.clone(), None)?;
        }
    }
    if analytics.objects.len() > OBJECT_COUNT_MAX {
        return Err(Error::new(Kind::Limit));
    }
    Ok(analytics)
}

fn parse_anpr(analytics: &mut Analytics, anpr: Node<'_>) -> Result<(), Error> {
    let mut object = object(anpr, None, Some(Target::Vehicle))?;
    object.plate = anpr.field("licensePlate")?;
    for picture in anpr.list("pictureInfoList", "pictureInfo")? {
        let image_id = image(&mut analytics.images, picture)?;
        if let Some(rectangle) = picture.child("plateRect")?
            && object.bbox.is_none()
        {
            object.bbox = Some(rect(rectangle, CoordinateSpace::Pixels)?);
            object.image_id = image_id;
        }
    }
    analytics.objects.push(object);
    Ok(())
}

fn add_object(
    output: &mut Analytics,
    node: Node<'_>,
    region: Option<String>,
    inherited: Option<Target>,
) -> Result<(), Error> {
    if output.objects.len() >= OBJECT_COUNT_MAX {
        return Err(Error::new(Kind::Limit));
    }
    output.objects.push(object(node, region, inherited)?);
    Ok(())
}

fn object(
    node: Node<'_>,
    region: Option<String>,
    inherited: Option<Target>,
) -> Result<Object, Error> {
    let explicit = classification(node)?;
    if explicit
        .zip(inherited)
        .is_some_and(|(explicit, inherited)| explicit != inherited)
    {
        return Err(Error::new(Kind::Protocol));
    }
    let confidence = node
        .field("confidenceLevel")?
        .map(|value| confidence(&value, 100.0))
        .transpose()?
        .or(node
            .field("confidence")?
            .map(|value| confidence(&value, 1.0))
            .transpose()?);
    let bbox = node
        .child("targetRect")?
        .map(|node| rect(node, CoordinateSpace::PerThousand))
        .transpose()?;
    let mut attributes = node.scalars()?;
    for name in ["humanInfo", "vehicleInfo"] {
        if let Some(info) = node.child(name)? {
            for (key, value) in info.scalars()? {
                if attributes.insert(key, value).is_some() {
                    return Err(Error::new(Kind::Protocol));
                }
            }
        }
    }
    Ok(Object {
        id: alias(node, &["targetID", "objectID", "id"])?,
        region,
        target: explicit.or(inherited),
        confidence,
        bbox,
        image_id: alias(node, &["contentID", "fileName", "pictureName"])?,
        plate: node.field("licensePlate")?,
        attributes,
    })
}

fn classification(node: Node<'_>) -> Result<Option<Target>, Error> {
    let mut target = None;
    for name in ["targetType", "detectionTarget"] {
        if let Some(value) = node.field(name)? {
            let current = match value.to_ascii_lowercase().as_str() {
                "human" | "person" => Some(Target::Person),
                "vehicle" => Some(Target::Vehicle),
                _ => None,
            };
            if target
                .zip(current)
                .is_some_and(|(target, current)| target != current)
            {
                return Err(Error::new(Kind::Protocol));
            }
            target = target.or(current);
        }
    }
    Ok(target)
}

fn confidence(value: &str, scale: f64) -> Result<Confidence, Error> {
    let value: f64 = value.parse().map_err(|_| Error::new(Kind::Protocol))?;
    if !value.is_finite() || !(0.0..=scale).contains(&value) {
        return Err(Error::new(Kind::Protocol));
    }
    Ok(Confidence(value / scale))
}

fn rect(node: Node<'_>, default: CoordinateSpace) -> Result<BoundingBox, Error> {
    let space = match node.field("coordinateSystem")?.as_deref() {
        None => default,
        Some("pixel" | "pixels") => CoordinateSpace::Pixels,
        Some("normalized") => CoordinateSpace::Normalized,
        Some("1000") => CoordinateSpace::PerThousand,
        _ => return Err(Error::new(Kind::Protocol)),
    };
    let mut values = [0.0; 4];
    for (index, names) in [
        ["X", "x"],
        ["Y", "y"],
        ["width", "width"],
        ["height", "height"],
    ]
    .iter()
    .enumerate()
    {
        let value = if names[0] == names[1] {
            node.field(names[0])?
        } else {
            alias(node, names)?
        }
        .ok_or_else(|| Error::new(Kind::Protocol))?;
        values[index] = value
            .parse::<f64>()
            .map_err(|_| Error::new(Kind::Protocol))?;
        if !values[index].is_finite() || values[index] < 0.0 || values[index] > f64::from(u32::MAX)
        {
            return Err(Error::new(Kind::Protocol));
        }
    }
    if values[2] <= 0.0 || values[3] <= 0.0 {
        return Err(Error::new(Kind::Protocol));
    }
    let rectangle = BoundingBox { values, space };
    if space != CoordinateSpace::Pixels {
        rectangle.normalized(None)?;
    }
    Ok(rectangle)
}

fn image(output: &mut Vec<ImageRef>, node: Node<'_>) -> Result<Option<String>, Error> {
    let id = alias(node, &["contentID", "fileName", "pictureName"])?
        .ok_or_else(|| Error::new(Kind::Protocol))?;
    add_image(output, id.clone(), node.field("type")?)?;
    Ok(Some(id))
}

fn add_image(output: &mut Vec<ImageRef>, id: String, kind: Option<String>) -> Result<(), Error> {
    if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
        return Err(Error::new(Kind::Protocol));
    }
    if let Some(previous) = output.iter().find(|image| image.id == id) {
        if kind.is_some() && previous.kind != kind {
            return Err(Error::new(Kind::Protocol));
        }
    } else {
        if output.len() >= IMAGE_COUNT_MAX {
            return Err(Error::new(Kind::Limit));
        }
        output.push(ImageRef { id, kind });
    }
    Ok(())
}

fn alias(node: Node<'_>, names: &[&str]) -> Result<Option<String>, Error> {
    let mut selected = None;
    for name in names {
        if let Some(value) = node.field(name)? {
            if selected.as_ref().is_some_and(|previous| previous != &value) {
                return Err(Error::new(Kind::Protocol));
            }
            selected = Some(value);
        }
    }
    Ok(selected)
}
