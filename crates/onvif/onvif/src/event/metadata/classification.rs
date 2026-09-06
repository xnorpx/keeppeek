use std::cmp::Ordering;

use xmltree::Element;

use super::{ObjectClass, ProtocolError, SCHEMA, tree, xml};

const CANDIDATE_COUNT_MAX: usize = 32;

pub(super) struct Classification {
    pub(super) class: Option<ObjectClass>,
    pub(super) confidence: Option<f32>,
}

pub(super) fn parse(appearance: &Element) -> Result<Classification, ProtocolError> {
    let explicit = explicit_class(appearance)?;
    let selected = match xml::child(appearance, SCHEMA, "Class")? {
        Some(class) => candidates(class)?,
        None => None,
    };
    Ok(selected.unwrap_or(Classification {
        class: explicit,
        confidence: None,
    }))
}

fn candidates(element: &Element) -> Result<Option<Classification>, ProtocolError> {
    let legacy = tree::children(element, "ClassCandidate").map(parse_candidate);
    let modern = tree::children(element, "Type").map(parse_type);
    let extension = xml::child(element, SCHEMA, "Extension")?;
    let extended = extension
        .into_iter()
        .flat_map(|extension| tree::children(extension, "OtherTypes"))
        .map(parse_candidate);
    let mut selected = None;
    for (index, candidate) in legacy.chain(modern).chain(extended).enumerate() {
        if index >= CANDIDATE_COUNT_MAX {
            return Err(ProtocolError("metadata class count exceeded"));
        }
        select(&mut selected, candidate?)?;
    }
    Ok(selected)
}

fn select(
    selected: &mut Option<Classification>,
    candidate: Classification,
) -> Result<(), ProtocolError> {
    let Some(previous) = selected.as_mut() else {
        *selected = Some(candidate);
        return Ok(());
    };
    match candidate.confidence.partial_cmp(&previous.confidence) {
        Some(Ordering::Greater) => *previous = candidate,
        Some(Ordering::Equal) if previous.class != candidate.class => previous.class = None,
        Some(_) => {}
        None => return Err(ProtocolError("invalid metadata confidence")),
    }
    Ok(())
}

fn parse_candidate(element: &Element) -> Result<Classification, ProtocolError> {
    let kind = tree::scalar(xml::required(element, SCHEMA, "Type")?)?;
    let confidence = tree::probability(&xml::field(element, SCHEMA, "Likelihood")?)?;
    Ok(Classification {
        class: category(&kind),
        confidence: Some(confidence),
    })
}

fn parse_type(element: &Element) -> Result<Classification, ProtocolError> {
    Ok(Classification {
        class: category(&tree::scalar(element)?),
        confidence: tree::likelihood(element)?,
    })
}

fn category(value: &str) -> Option<ObjectClass> {
    match value {
        "Human" | "HumanBody" | "Person" => Some(ObjectClass::Person),
        "Vehicle" | "Vehical" | "Car" | "Bus" | "Truck" | "Bicycle" | "Motorcycle" | "Bike" => {
            Some(ObjectClass::Vehicle)
        }
        "Animal" => Some(ObjectClass::Animal),
        "HumanFace" | "Face" => Some(ObjectClass::Face),
        "LicensePlate" => Some(ObjectClass::LicensePlate),
        "Package" => Some(ObjectClass::Package),
        _ => None,
    }
}

fn explicit_class(appearance: &Element) -> Result<Option<ObjectClass>, ProtocolError> {
    let body = xml::child(appearance, SCHEMA, "HumanBody")?.is_some();
    let face = xml::child(appearance, SCHEMA, "HumanFace")?.is_some();
    let plate = xml::child(appearance, SCHEMA, "LicensePlateInfo")?.is_some();
    let mut vehicle = false;
    for (index, info) in tree::children(appearance, "VehicleInfo").enumerate() {
        if index >= CANDIDATE_COUNT_MAX {
            return Err(ProtocolError("metadata vehicle count exceeded"));
        }
        parse_type(xml::required(info, SCHEMA, "Type")?)?;
        vehicle = true;
    }
    if (body || face) && (vehicle || plate) {
        return Ok(None);
    }
    Ok(if body {
        Some(ObjectClass::Person)
    } else if face {
        Some(ObjectClass::Face)
    } else if vehicle {
        Some(ObjectClass::Vehicle)
    } else if plate {
        Some(ObjectClass::LicensePlate)
    } else {
        None
    })
}
