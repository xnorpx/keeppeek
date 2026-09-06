use super::{Query, boolean, nonzero, number, replacement, required};
use crate::error::Kind;
use crate::{Document, Error, Request};

/// Validated device identity with its full response retained.
#[derive(Clone, Debug)]
pub struct DeviceInfo {
    document: Document,
    model: String,
    name: Option<String>,
    serial: Option<String>,
    firmware: Option<String>,
}

impl DeviceInfo {
    /// Creates the standard read-only device identity query.
    ///
    /// # Errors
    /// Returns request validation errors.
    pub fn query() -> Result<Query<Self>, Error> {
        Query::new("/ISAPI/System/deviceInfo", Self::parse)
    }
    fn parse(document: Document) -> Result<Self, Error> {
        let root = document.root("DeviceInfo")?;
        Ok(Self {
            model: required(root, "model")?,
            name: root.field("deviceName")?,
            serial: root.field("serialNumber")?,
            firmware: root.field("firmwareVersion")?,
            document,
        })
    }
    /// Returns the reported model without vendor inference.
    pub fn model(&self) -> &str {
        &self.model
    }
    /// Returns the configured device name.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
    /// Returns the reported serial number.
    pub fn serial(&self) -> Option<&str> {
        self.serial.as_deref()
    }
    /// Returns the reported firmware version.
    pub fn firmware(&self) -> Option<&str> {
        self.firmware.as_deref()
    }
    /// Returns unknown identity extensions as well as the typed fields.
    pub const fn document(&self) -> &Document {
        &self.document
    }
}

/// A device clock's explicit synchronization mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeMode {
    /// The caller sets the clock explicitly.
    Manual,
    /// The device uses its configured NTP servers.
    Ntp,
}

/// A clock configuration retained for explicit read/modify/write operations.
#[derive(Clone, Debug)]
pub struct Time {
    document: Document,
    mode: TimeMode,
    local: Option<String>,
    zone: Option<String>,
}

impl Time {
    /// Creates the read-only clock query.
    ///
    /// # Errors
    /// Returns request validation errors.
    pub fn query() -> Result<Query<Self>, Error> {
        Query::new("/ISAPI/System/time", Self::parse)
    }
    fn parse(document: Document) -> Result<Self, Error> {
        let root = document.root("Time")?;
        let mode = match required(root, "timeMode")?.as_str() {
            "manual" => TimeMode::Manual,
            "NTP" | "ntp" => TimeMode::Ntp,
            _ => return Err(Error::new(Kind::Protocol)),
        };
        let local = root.field("localTime")?;
        if let Some(local) = &local {
            chrono::DateTime::parse_from_rfc3339(local).map_err(|_| Error::new(Kind::Protocol))?;
        }
        Ok(Self {
            mode,
            local,
            zone: root.field("timeZone")?,
            document,
        })
    }
    /// Returns the validated clock mode.
    pub const fn mode(&self) -> TimeMode {
        self.mode
    }
    /// Returns the device's original timestamp including its offset.
    pub fn local_time(&self) -> Option<&str> {
        self.local.as_deref()
    }
    /// Returns the camera's timezone syntax without translating it.
    pub fn zone(&self) -> Option<&str> {
        self.zone.as_deref()
    }
    /// Sets the clock explicitly without sending a request.
    ///
    /// # Errors
    /// Rejects timestamps that are not RFC 3339 or timezones over 128 bytes.
    pub fn set(&mut self, mode: TimeMode, local: &str, zone: &str) -> Result<(), Error> {
        chrono::DateTime::parse_from_rfc3339(local).map_err(|_| Error::new(Kind::InvalidInput))?;
        if zone.is_empty() || zone.len() > 128 || zone.chars().any(char::is_control) {
            return Err(Error::new(Kind::InvalidInput));
        }
        let mut document = self.document.clone();
        document.set(
            "Time",
            &["timeMode"],
            match mode {
                TimeMode::Manual => "manual",
                TimeMode::Ntp => "NTP",
            }
            .into(),
        )?;
        document.set("Time", &["localTime"], local.into())?;
        document.set("Time", &["timeZone"], zone.into())?;
        self.document = document;
        self.mode = mode;
        self.local = Some(local.to_owned());
        self.zone = Some(zone.to_owned());
        Ok(())
    }
    /// Builds an explicit replacement while preserving unknown clock fields.
    ///
    /// # Errors
    /// Rejects serialized bodies exceeding the request limit.
    pub fn update(&self) -> Result<Request, Error> {
        replacement(&self.document, "/ISAPI/System/time".to_owned())
    }
}

/// A motion configuration that preserves masks, schedules and vendor extensions.
#[derive(Clone, Debug)]
pub struct Motion {
    document: Document,
    enabled: bool,
    sensitivity: Option<u32>,
}

impl Motion {
    /// Queries one video input channel's motion configuration.
    ///
    /// # Errors
    /// Rejects channel zero.
    pub fn query(channel: u32) -> Result<Query<Self>, Error> {
        Query::new(
            format!(
                "/ISAPI/System/Video/inputs/channels/{}/motionDetection",
                nonzero(channel)?
            ),
            Self::parse,
        )
    }
    fn parse(document: Document) -> Result<Self, Error> {
        let root = document.root("MotionDetection")?;
        let sensitivity = number(root, "sensitivityLevel")?;
        if sensitivity.is_some_and(|value| value > 100) {
            return Err(Error::new(Kind::Protocol));
        }
        Ok(Self {
            enabled: boolean(root, "enabled")?,
            sensitivity,
            document,
        })
    }
    /// Returns the enabled state read from the device.
    pub const fn enabled(&self) -> bool {
        self.enabled
    }
    /// Returns the sensitivity, if this model reports one.
    pub const fn sensitivity(&self) -> Option<u32> {
        self.sensitivity
    }
    /// Changes only the enabled state in the retained configuration.
    ///
    /// # Errors
    /// Rejects an invalid stored document.
    pub fn set_enabled(&mut self, enabled: bool) -> Result<(), Error> {
        self.document
            .set("MotionDetection", &["enabled"], enabled.into())?;
        self.enabled = enabled;
        Ok(())
    }
    /// Changes only sensitivity, preserving region masks and unknown fields.
    ///
    /// # Errors
    /// Rejects values above 100.
    pub fn set_sensitivity(&mut self, sensitivity: u32) -> Result<(), Error> {
        if sensitivity > 100 {
            return Err(Error::new(Kind::InvalidInput));
        }
        self.document
            .set("MotionDetection", &["sensitivityLevel"], sensitivity.into())?;
        self.sensitivity = Some(sensitivity);
        Ok(())
    }
    /// Builds an explicit replacement for the selected input channel.
    ///
    /// # Errors
    /// Rejects channel zero and oversized serialized bodies.
    pub fn update(&self, channel: u32) -> Result<Request, Error> {
        replacement(
            &self.document,
            Self::query(channel)?.request.resource().to_owned(),
        )
    }
    /// Returns the retained configuration, including masks and vendor fields.
    pub const fn document(&self) -> &Document {
        &self.document
    }
}

/// A streaming channel with retained encoder and transport extensions.
#[derive(Clone, Debug)]
pub struct Stream {
    document: Document,
    id: u32,
}

impl Stream {
    /// Lists streaming channels while rejecting duplicate device IDs.
    ///
    /// # Errors
    /// Rejects invalid responses or lists over 512 channels.
    pub fn list() -> Result<Query<Vec<Self>>, Error> {
        Query::new("/ISAPI/Streaming/channels", |document| {
            let nodes = document
                .root("StreamingChannelList")?
                .children("StreamingChannel");
            if nodes.len() > 512 {
                return Err(Error::new(Kind::Limit));
            }
            let mut ids = std::collections::BTreeSet::new();
            nodes
                .into_iter()
                .map(|node| {
                    let stream = Self::parse(node.document())?;
                    if !ids.insert(stream.id) {
                        return Err(Error::new(Kind::Protocol));
                    }
                    Ok(stream)
                })
                .collect()
        })
    }
    /// Queries a stream ID such as 101 (main) or 102 (sub).
    ///
    /// # Errors
    /// Rejects stream zero.
    pub fn query(stream: u32) -> Result<Query<Self>, Error> {
        Query::new(
            format!("/ISAPI/Streaming/channels/{}", nonzero(stream)?),
            Self::parse,
        )
    }
    fn parse(document: Document) -> Result<Self, Error> {
        let root = document.root("StreamingChannel")?;
        let id = number(root, "id")?
            .filter(|id| *id > 0)
            .ok_or_else(|| Error::new(Kind::Protocol))?;
        Ok(Self { id, document })
    }
    /// Returns the stream's explicit device ID.
    pub const fn id(&self) -> u32 {
        self.id
    }
    /// Returns the retained channel configuration.
    pub const fn document(&self) -> &Document {
        &self.document
    }
    /// Sets validated encoder dimensions, bitrate and frame rate while preserving extensions.
    ///
    /// # Errors
    /// Rejects dimensions outside 1..8192, bitrate outside 1..100000 Kbps and frame rates outside 1..12000 hundredths of a frame per second.
    pub fn set_video(
        &mut self,
        width: u32,
        height: u32,
        bitrate_kbps: u32,
        frame_rate_hundredths: u32,
    ) -> Result<(), Error> {
        if !(1..=8192).contains(&width)
            || !(1..=8192).contains(&height)
            || !(1..=100_000).contains(&bitrate_kbps)
            || !(1..=12_000).contains(&frame_rate_hundredths)
        {
            return Err(Error::new(Kind::InvalidInput));
        }
        let mut document = self.document.clone();
        for (name, value) in [
            ("videoResolutionWidth", width),
            ("videoResolutionHeight", height),
            ("constantBitRate", bitrate_kbps),
            ("maxFrameRate", frame_rate_hundredths),
        ] {
            document.set("StreamingChannel", &["Video", name], value.into())?;
        }
        self.document = document;
        Ok(())
    }
    /// Replaces this stream explicitly, with no network retry policy.
    ///
    /// # Errors
    /// Rejects serialized bodies above the request limit.
    pub fn update(&self) -> Result<Request, Error> {
        replacement(
            &self.document,
            Self::query(self.id)?.request.resource().to_owned(),
        )
    }
}
