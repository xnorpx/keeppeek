use crate::event::{ObjectClass, ProtocolError, Topic};

use super::{Kind, TOPICS};

pub(super) struct Mapping {
    kind: Option<Kind>,
    pub(super) states: &'static [&'static str],
    pub(super) point: bool,
}

pub(super) fn lookup(topic: &Topic) -> Result<Option<Mapping>, ProtocolError> {
    if topic.path.len() > crate::event::TOPIC_SEGMENT_COUNT_MAX {
        return Err(ProtocolError("normalization topic limit exceeded"));
    }
    if topic
        .path
        .first()
        .and_then(|name| name.namespace_uri.as_deref())
        != Some(TOPICS)
        || topic.path.iter().any(|name| {
            name.namespace_uri
                .as_deref()
                .is_some_and(|namespace| namespace != TOPICS)
        })
    {
        return Ok(None);
    }
    let names: Vec<_> = topic
        .path
        .iter()
        .map(|name| name.local_name.as_str())
        .collect();
    Ok(lookup_path(&names))
}

fn lookup_path(names: &[&str]) -> Option<Mapping> {
    let mapping = match names {
        ["RuleEngine", "CellMotionDetector", "Motion"] => {
            property(Kind::Motion, &["IsMotion", "State"])
        }
        ["RuleEngine", "MotionRegionDetector", "Motion"] | ["VideoSource", "MotionAlarm"] => {
            property(Kind::Motion, &["State", "IsMotion"])
        }
        ["VideoSource", "Tamper"] => property(Kind::Tamper, &["State", "IsTamper"]),
        ["Device", "Trigger", "DigitalInput"] => {
            property(Kind::DigitalInput, &["LogicalState", "State"])
        }
        ["AudioSource", "AudioDetection"] => property(
            Kind::AudioDetected,
            &["State", "IsSoundDetected", "IsSound"],
        ),
        ["VideoSource", "SignalLoss"] => property(Kind::VideoLoss, &["State", "IsSignalLoss"]),
        ["RuleEngine", "LineDetector", "Crossed"] => point(Kind::LineCrossing),
        ["RuleEngine", "FieldDetector", "ObjectsInside"] => {
            property(Kind::Intrusion, &["IsInside", "State"])
        }
        ["RuleEngine", "FieldDetector", "RegionEntrance"] => point(Kind::RegionEntry),
        ["RuleEngine", "FieldDetector", "RegionExit"] => point(Kind::RegionExit),
        ["RuleEngine", "FieldDetector", "Loitering"]
        | ["RuleEngine", "LoiteringDetector", "ObjectIsLoitering"] => {
            property(Kind::Loitering, &["State", "IsLoitering"])
        }
        ["RuleEngine", "ObjectDetector", "Count"]
        | [
            "RuleEngine",
            "CountAggregation",
            "Counter" | "OccupancyCounter",
        ] => point(Kind::ObjectCount),
        ["RuleEngine", "ObjectDetection", "Object"] => Mapping {
            kind: None,
            states: &["State", "IsDetected"],
            point: true,
        },
        ["RuleEngine", "ObjectDetector", category] => {
            let kind = object_kind(category)?;
            property(kind, &["State", "IsDetected"])
        }
        ["RuleEngine", "Recognition", "Face"] => point(Kind::Face),
        ["RuleEngine", "Recognition", "LicensePlate"] => point(Kind::LicensePlate),
        ["RuleEngine", "MyRuleDetector", "Visitor"] => point(Kind::DoorbellPress),
        _ => return None,
    };
    Some(mapping)
}

const fn property(kind: Kind, states: &'static [&'static str]) -> Mapping {
    Mapping {
        kind: Some(kind),
        states,
        point: false,
    }
}

const fn point(kind: Kind) -> Mapping {
    Mapping {
        kind: Some(kind),
        states: &["State", "IsDetected"],
        point: true,
    }
}

impl Mapping {
    pub(super) fn classified_kind(&self, class: Option<ObjectClass>) -> Option<Kind> {
        match self.kind {
            Some(
                kind @ (Kind::Motion
                | Kind::LineCrossing
                | Kind::Intrusion
                | Kind::RegionEntry
                | Kind::RegionExit
                | Kind::Loitering),
            ) => Some(class.map_or(kind, Kind::from)),
            Some(kind) => Some(kind),
            None => class.map(Kind::from),
        }
    }
}

fn object_kind(category: &str) -> Option<Kind> {
    match category {
        "Person" | "Human" => Some(Kind::Person),
        "Vehicle" => Some(Kind::Vehicle),
        "Animal" => Some(Kind::Animal),
        "Face" => Some(Kind::Face),
        "LicensePlate" => Some(Kind::LicensePlate),
        "Package" => Some(Kind::Package),
        _ => None,
    }
}
