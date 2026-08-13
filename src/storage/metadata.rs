use serde::{Deserialize, Serialize};

/// Origin of an event shown in recorded-video review.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    Camera,
    KeepPeek,
}

impl EventSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Camera => "camera",
            Self::KeepPeek => "keeppeek",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "camera" => Some(Self::Camera),
            "keeppeek" => Some(Self::KeepPeek),
            _ => None,
        }
    }
}

/// A camera or KeepPeek detection event aligned to the recording clock.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TimelineEvent {
    pub id: String,
    pub camera_id: String,
    pub stream: Option<String>,
    pub source: EventSource,
    pub kind: String,
    pub start_time_ms: i64,
    pub end_time_ms: Option<i64>,
    pub confidence: Option<f64>,
    pub bbox: Option<[f32; 4]>,
    pub zone: Option<String>,
    pub thumbnail_filename: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CameraMetadata {
    pub camera_id: String,

    #[serde(default)]
    pub zones: Vec<Zone>,

    #[serde(default)]
    pub static_objects: Vec<StaticObject>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Zone {
    pub name: String,
    pub polygon: Vec<[f32; 2]>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StaticObject {
    pub name: String,
    pub bbox: [f32; 4],
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Event {
    pub event_type: String,
    pub confidence: u8,
    pub bbox: [f32; 4],

    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<u16>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub zone_id: Option<u16>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Keyframe {
    pub timestamp: u64,
    pub file_offset: u64,

    #[serde(default)]
    pub events: Vec<Event>,
}
