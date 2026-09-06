use std::fmt;

use crate::error::Kind;
use crate::{Error, XML_SIZE_BYTES_MAX};

const RESOURCE_SIZE_BYTES_MAX: usize = 4096;
const MOTION_DURATION_MS_MAX: u32 = 10_000;

/// HTTP methods supported by the ISAPI request layer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Method {
    /// Read a resource without changing camera configuration.
    Get,
    /// Replace a resource or execute an explicitly requested camera command.
    Put,
    /// Create a resource or run an explicit operation.
    Post,
    /// Delete a resource explicitly selected by the caller.
    Delete,
}

impl Method {
    /// Returns the uppercase HTTP method token.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Put => "PUT",
            Self::Post => "POST",
            Self::Delete => "DELETE",
        }
    }
}

impl fmt::Display for Method {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// An owned, origin-relative ISAPI request with a bounded body.
#[derive(Clone, Eq, PartialEq)]
pub struct Request {
    method: Method,
    resource: String,
    body: Vec<u8>,
    format: Option<Format>,
}

/// The media format of a structured request body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Format {
    /// UTF-8 XML with normal XML escaping.
    Xml,
    /// UTF-8 JSON with normal JSON escaping.
    Json,
    /// Original binary media bytes, without XML or JSON conversion.
    Binary,
}

impl Format {
    /// Returns the HTTP Content-Type for this body.
    pub const fn content_type(self) -> &'static str {
        match self {
            Self::Xml => "application/xml; charset=utf-8",
            Self::Json => "application/json; charset=utf-8",
            Self::Binary => "application/octet-stream",
        }
    }
}

impl Request {
    /// Constructs a read-only request without performing I/O.
    ///
    /// # Errors
    /// Rejects non-ISAPI paths, fragments, normalization changes, and oversized paths.
    pub fn get(resource: impl AsRef<str>) -> Result<Self, Error> {
        let resource = resource.as_ref();
        validate_resource(resource)?;
        Ok(Self {
            method: Method::Get,
            resource: resource.to_owned(),
            body: Vec::new(),
            format: None,
        })
    }

    /// Constructs an XML replacement request without performing I/O.
    ///
    /// # Errors
    /// Rejects invalid paths and bodies exceeding [`XML_SIZE_BYTES_MAX`].
    pub fn put(resource: impl AsRef<str>, body: impl AsRef<[u8]>) -> Result<Self, Error> {
        Self::with_body(Method::Put, resource, Format::Xml, body)
    }

    /// Constructs a bounded structured request for an explicit HTTP method.
    ///
    /// # Errors
    /// Rejects GET bodies, invalid resource paths and excessive body sizes.
    pub fn with_body(
        method: Method,
        resource: impl AsRef<str>,
        format: Format,
        body: impl AsRef<[u8]>,
    ) -> Result<Self, Error> {
        let body = body.as_ref();
        if body.len() > XML_SIZE_BYTES_MAX {
            return Err(Error::new(Kind::Limit));
        }
        let mut request = Self::get(resource)?;
        if method == Method::Get {
            return Err(Error::new(Kind::InvalidInput));
        }
        request.method = method;
        request.body = body.to_vec();
        request.format = Some(format);
        Ok(request)
    }

    /// Creates an empty-body deletion request.
    ///
    /// # Errors
    /// Returns the resource validation errors from [`Self::get`].
    pub fn delete(resource: impl AsRef<str>) -> Result<Self, Error> {
        let mut request = Self::get(resource)?;
        request.method = Method::Delete;
        Ok(request)
    }

    /// Returns the body media type, when this request has a structured body.
    pub fn content_type(&self) -> Option<&'static str> {
        self.format.map(Format::content_type)
    }

    /// Returns the HTTP method the transport must send.
    pub const fn method(&self) -> Method {
        self.method
    }

    /// Returns the validated path and optional query string used for Digest signing.
    pub fn resource(&self) -> &str {
        &self.resource
    }

    /// Returns the exact request bytes, including any XML encoding declaration.
    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

impl fmt::Debug for Request {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Request")
            .field("method", &self.method)
            .field("body_size_bytes", &self.body.len())
            .finish_non_exhaustive()
    }
}

fn validate_resource(resource: &str) -> Result<(), Error> {
    if resource.len() > RESOURCE_SIZE_BYTES_MAX {
        return Err(Error::new(Kind::Limit));
    }
    if !resource.starts_with("/ISAPI/")
        || !resource.is_ascii()
        || resource
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b' ')
        || resource.contains(['#', '\\'])
    {
        return Err(Error::new(Kind::InvalidInput));
    }
    let url = url::Url::parse(&format!("http://isapi.invalid{resource}"))
        .map_err(|_| Error::new(Kind::InvalidInput))?;
    if &url[url::Position::BeforePath..] != resource {
        return Err(Error::new(Kind::InvalidInput));
    }
    Ok(())
}

/// Validated pan, tilt, and zoom speeds in the camera's signed percentage units.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Ptz {
    pan: i8,
    tilt: i8,
    zoom: i8,
}

impl Ptz {
    /// Creates continuous motion. The caller must send a zero-speed stop command.
    ///
    /// # Errors
    /// Rejects channel zero.
    pub fn continuous(self, channel: u32) -> Result<Request, Error> {
        if channel == 0 {
            return Err(Error::new(Kind::InvalidInput));
        }
        Request::put(
            format!("/ISAPI/PTZCtrl/channels/{channel}/continuous"),
            format!(
                "<PTZData><pan>{}</pan><tilt>{}</tilt><zoom>{}</zoom></PTZData>",
                self.pan, self.tilt, self.zoom
            ),
        )
    }

    /// Creates an absolute move in tenths of degrees and camera-specific zoom units.
    ///
    /// # Errors
    /// Rejects zero channels, elevation outside -900..900 and azimuth above 3600.
    pub fn absolute(
        channel: u32,
        elevation: i16,
        azimuth: u16,
        zoom: u16,
    ) -> Result<Request, Error> {
        if channel == 0 || !(-900..=900).contains(&elevation) || azimuth > 3600 {
            return Err(Error::new(Kind::InvalidInput));
        }
        Request::put(
            format!("/ISAPI/PTZCtrl/channels/{channel}/absolute"),
            format!(
                "<PTZData><AbsoluteHigh><elevation>{elevation}</elevation><azimuth>{azimuth}</azimuth><absoluteZoom>{zoom}</absoluteZoom></AbsoluteHigh></PTZData>"
            ),
        )
    }

    /// Creates an explicit recall of an existing preset.
    ///
    /// # Errors
    /// Rejects zero channel or preset IDs.
    pub fn goto_preset(channel: u32, preset: u32) -> Result<Request, Error> {
        if channel == 0 || preset == 0 {
            return Err(Error::new(Kind::InvalidInput));
        }
        Request::put(
            format!("/ISAPI/PTZCtrl/channels/{channel}/presets/{preset}/goto"),
            [],
        )
    }
    /// Validates three independently controlled axes without moving the camera.
    ///
    /// # Errors
    /// Each speed must be between -100 and 100, inclusive.
    pub fn new(pan: i8, tilt: i8, zoom: i8) -> Result<Self, Error> {
        if [pan, tilt, zoom]
            .iter()
            .any(|speed| !(-100..=100).contains(speed))
        {
            return Err(Error::new(Kind::InvalidInput));
        }
        Ok(Self { pan, tilt, zoom })
    }

    /// Creates a momentary movement request; the caller controls when it is sent.
    ///
    /// # Errors
    /// Requires a nonzero channel and a duration between 1 and 10,000 milliseconds.
    pub fn momentary(self, channel: u32, duration_ms: u32) -> Result<Request, Error> {
        if channel == 0 || !(1..=MOTION_DURATION_MS_MAX).contains(&duration_ms) {
            return Err(Error::new(Kind::InvalidInput));
        }
        let body = format!(
            "<PTZData><pan>{}</pan><tilt>{}</tilt><zoom>{}</zoom><Momentary><duration>{duration_ms}</duration></Momentary></PTZData>",
            self.pan, self.tilt, self.zoom
        );
        Request::put(format!("/ISAPI/PTZCtrl/channels/{channel}/Momentary"), body)
    }
}
