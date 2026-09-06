use std::cmp::Ordering;

use crate::event::{ItemList, ObjectClass, ProtocolError, XmlElement};

use super::details::{attribute, child, matches, probability, scalar, structured};

const CANDIDATE_COUNT_MAX: usize = 32;
const CLASS_SIZE_BYTES_MAX: usize = 256;

#[derive(Default)]
pub(super) struct Classification {
    pub(super) class: Option<ObjectClass>,
    pub(super) confidence: Option<f32>,
}

pub(super) fn parse(
    data: &ItemList,
    appearance: Option<&XmlElement>,
) -> Result<Classification, ProtocolError> {
    let mut candidates = Candidates::default();
    for item in &data.simple {
        match item.name.as_str() {
            "Class" | "ObjectClass" | "ClassType" => candidates.add(&item.value, None)?,
            "ClassTypes" => {
                for value in item.value.split_ascii_whitespace() {
                    candidates.add(value, None)?;
                }
            }
            _ => {}
        }
    }
    if let Some(class) = structured(data, "Class", &["Class"]) {
        candidates.element(class)?;
    }
    if let Some(appearance) = appearance
        && let Some(class) = child(appearance, "Class")?
    {
        candidates.element(class)?;
    }
    Ok(candidates.selected.unwrap_or_default())
}

#[derive(Default)]
struct Candidates {
    count: usize,
    selected: Option<Classification>,
}

impl Candidates {
    fn add(&mut self, value: &str, confidence: Option<f32>) -> Result<(), ProtocolError> {
        if self.count >= CANDIDATE_COUNT_MAX || value.len() > CLASS_SIZE_BYTES_MAX {
            return Err(ProtocolError("normalization class limit exceeded"));
        }
        self.count += 1;
        let candidate = Classification {
            class: category(value.trim()),
            confidence,
        };
        if let Some(previous) = self.selected.as_mut() {
            match candidate.confidence.partial_cmp(&previous.confidence) {
                Some(Ordering::Greater) => *previous = candidate,
                Some(Ordering::Equal) if candidate.class != previous.class => previous.class = None,
                Some(_) => {}
                None => return Err(ProtocolError("invalid normalization class likelihood")),
            }
        } else {
            self.selected = Some(candidate);
        }
        Ok(())
    }

    fn element(&mut self, element: &XmlElement) -> Result<(), ProtocolError> {
        for candidate in &element.children {
            if matches(candidate, "Type") {
                let confidence = attribute(candidate, "Likelihood")
                    .map(probability)
                    .transpose()?;
                self.add(scalar(candidate)?, confidence)?;
            } else if matches(candidate, "ClassCandidate") {
                let kind = child(candidate, "Type")?
                    .ok_or(ProtocolError("missing normalization class type"))?;
                let likelihood = child(candidate, "Likelihood")?
                    .ok_or(ProtocolError("missing normalization class likelihood"))?;
                self.add(scalar(kind)?, Some(probability(scalar(likelihood)?)?))?;
            }
        }
        Ok(())
    }
}

fn category(value: &str) -> Option<ObjectClass> {
    let categories: &[(&[&str], ObjectClass)] = &[
        (&["Person", "Human", "HumanBody"], ObjectClass::Person),
        (
            &[
                "Vehicle",
                "Vehical",
                "Car",
                "Bus",
                "Truck",
                "Bicycle",
                "Motorcycle",
                "Bike",
            ],
            ObjectClass::Vehicle,
        ),
        (&["Animal"], ObjectClass::Animal),
        (&["Face", "HumanFace"], ObjectClass::Face),
        (&["LicensePlate"], ObjectClass::LicensePlate),
        (&["Package"], ObjectClass::Package),
    ];
    categories
        .iter()
        .find(|(names, _)| names.iter().any(|name| value.eq_ignore_ascii_case(name)))
        .map(|(_, class)| *class)
}
