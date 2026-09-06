use super::{Query, nonzero, number, required};

/// Reported PTZ movement spaces and preset capacity, independent of application support.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PtzCapabilities {
    pan: Option<[i8; 2]>,
    tilt: Option<[i8; 2]>,
    zoom: Option<[i8; 2]>,
    max_presets: u32,
    home: bool,
}

impl PtzCapabilities {
    /// Queries the selected channel's advertised movement spaces.
    ///
    /// # Errors
    /// Rejects channel zero, invalid ranges and wrong capability documents.
    pub fn query(channel: u32) -> Result<Query<Self>, Error> {
        Query::new(
            format!("/ISAPI/PTZCtrl/channels/{}/capabilities", nonzero(channel)?),
            Self::parse,
        )
    }

    fn parse(document: Document) -> Result<Self, Error> {
        let root = document
            .root("PTZChanelCap")
            .or_else(|_| document.root("PTZChannelCap"))?;
        let continuous = root.child("ContinuousPanTiltSpace")?;
        let range = |parent: Option<crate::document::Node<'_>>,
                     name: &str|
         -> Result<Option<[i8; 2]>, Error> {
            let Some(parent) = parent else {
                return Ok(None);
            };
            let node = parent
                .child(name)?
                .ok_or_else(|| Error::new(Kind::Protocol))?;
            let min = required(node, "Min")?
                .parse::<i8>()
                .map_err(|_| Error::new(Kind::Protocol))?;
            let max = required(node, "Max")?
                .parse::<i8>()
                .map_err(|_| Error::new(Kind::Protocol))?;
            if !(-100..=0).contains(&min) || !(0..=100).contains(&max) || min > max {
                return Err(Error::new(Kind::Protocol));
            }
            Ok((min != max).then_some([min, max]))
        };
        let max_presets =
            number(root, "maxPresetNum")?.ok_or_else(|| Error::new(Kind::Protocol))?;
        if max_presets > 10_000 {
            return Err(Error::new(Kind::Limit));
        }
        Ok(Self {
            pan: range(continuous, "XRange")?,
            tilt: range(continuous, "YRange")?,
            zoom: range(root.child("ContinuousZoomSpace")?, "ZRange")?,
            max_presets,
            home: super::boolean(root, "homePostionSupport")?,
        })
    }

    /// Returns whether pan or tilt has a nonzero continuous movement range.
    pub const fn continuous_pan_tilt(&self) -> bool {
        self.pan.is_some() || self.tilt.is_some()
    }
    /// Returns whether zoom has a nonzero continuous movement range.
    pub const fn continuous_zoom(&self) -> bool {
        self.zoom.is_some()
    }
    /// Returns signed minimum/maximum pan speeds in device percentage units.
    pub const fn pan_range(&self) -> Option<[i8; 2]> {
        self.pan
    }
    /// Returns signed minimum/maximum tilt speeds in device percentage units.
    pub const fn tilt_range(&self) -> Option<[i8; 2]> {
        self.tilt
    }
    /// Returns signed minimum/maximum zoom speeds in device percentage units.
    pub const fn zoom_range(&self) -> Option<[i8; 2]> {
        self.zoom
    }
    /// Returns the advertised number of presets, not a maximum preset identifier.
    pub const fn max_presets(&self) -> u32 {
        self.max_presets
    }
    /// Returns whether the camera advertises a home position.
    pub const fn home_supported(&self) -> bool {
        self.home
    }
}
use crate::error::Kind;
use crate::{Document, Error, Request};

/// A validated PTZ position in device units.
#[derive(Clone, Debug)]
pub struct PtzStatus {
    document: Document,
    azimuth: Option<u32>,
    elevation: Option<i32>,
    zoom: Option<u32>,
}

impl PtzStatus {
    /// Queries the selected PTZ channel.
    ///
    /// # Errors
    /// Rejects channel zero.
    pub fn query(channel: u32) -> Result<Query<Self>, Error> {
        Query::new(
            format!("/ISAPI/PTZCtrl/channels/{}/status", nonzero(channel)?),
            Self::parse,
        )
    }
    fn parse(document: Document) -> Result<Self, Error> {
        let root = document.root("PTZStatus")?;
        let absolute = root.child("AbsoluteHigh")?.unwrap_or(root);
        let azimuth = number(absolute, "azimuth")?;
        let elevation = absolute
            .field("elevation")?
            .map(|value| value.parse::<i32>().map_err(|_| Error::new(Kind::Protocol)))
            .transpose()?;
        if azimuth.is_some_and(|value| value > 3600)
            || elevation.is_some_and(|value| !(-900..=900).contains(&value))
        {
            return Err(Error::new(Kind::Protocol));
        }
        Ok(Self {
            azimuth,
            elevation,
            zoom: number(absolute, "absoluteZoom")?,
            document,
        })
    }
    /// Returns azimuth in tenths of a degree.
    pub const fn azimuth(&self) -> Option<u32> {
        self.azimuth
    }
    /// Returns elevation in tenths of a degree.
    pub const fn elevation(&self) -> Option<i32> {
        self.elevation
    }
    /// Returns zoom in camera-specific units.
    pub const fn zoom(&self) -> Option<u32> {
        self.zoom
    }
    /// Returns the validated status including vendor extensions.
    pub const fn document(&self) -> &Document {
        &self.document
    }
}

/// A named preset with a stable device ID.
#[derive(Clone, Debug)]
pub struct Preset {
    id: u32,
    name: String,
    document: Document,
}

impl Preset {
    /// Constructs a preset definition without moving or writing the camera.
    ///
    /// # Errors
    /// Rejects zero IDs, empty names and names longer than 128 bytes.
    pub fn new(id: u32, name: &str) -> Result<Self, Error> {
        nonzero(id)?;
        if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
            return Err(Error::new(Kind::InvalidInput));
        }
        let mut document = Document::empty_xml("PTZPreset")?;
        for (key, value) in [
            ("id", id.to_string()),
            ("presetName", name.to_owned()),
            ("enabled", "true".to_owned()),
        ] {
            document.set("PTZPreset", &[key], value.into())?;
        }
        Ok(Self {
            id,
            name: name.to_owned(),
            document,
        })
    }
    /// Lists the presets for a selected PTZ channel.
    ///
    /// # Errors
    /// Rejects channel zero and invalid device responses.
    pub fn list(channel: u32) -> Result<Query<Vec<Self>>, Error> {
        Query::new(
            format!("/ISAPI/PTZCtrl/channels/{}/presets", nonzero(channel)?),
            |document| {
                let nodes = document.root("PTZPresetList")?.children("PTZPreset");
                let mut ids = std::collections::BTreeSet::new();
                if nodes.len() > 512 {
                    return Err(Error::new(Kind::Limit));
                }
                nodes
                    .into_iter()
                    .map(|node| {
                        let id = number(node, "id")?
                            .filter(|id| *id > 0)
                            .ok_or_else(|| Error::new(Kind::Protocol))?;
                        if !ids.insert(id) {
                            return Err(Error::new(Kind::Protocol));
                        }
                        Ok(Self {
                            id,
                            name: required(node, "presetName")?,
                            document: node.document(),
                        })
                    })
                    .collect()
            },
        )
    }
    /// Returns the device preset ID.
    pub const fn id(&self) -> u32 {
        self.id
    }
    /// Returns the configured preset name.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Stores this preset at the camera's current position.
    ///
    /// # Errors
    /// Rejects channel zero and serialization failures.
    pub fn store(&self, channel: u32) -> Result<Request, Error> {
        Request::put(
            format!(
                "/ISAPI/PTZCtrl/channels/{}/presets/{}",
                nonzero(channel)?,
                self.id
            ),
            self.document.to_bytes()?,
        )
    }
    /// Deletes one explicit preset without moving the camera.
    ///
    /// # Errors
    /// Rejects channel or preset zero.
    pub fn delete(channel: u32, preset: u32) -> Result<Request, Error> {
        Request::delete(format!(
            "/ISAPI/PTZCtrl/channels/{}/presets/{}",
            nonzero(channel)?,
            nonzero(preset)?
        ))
    }
}
