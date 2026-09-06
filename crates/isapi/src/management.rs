//! Typed ISAPI queries and explicit configuration commands.
//!
//! Queries validate the expected resource and device-level status. Configuration
//! objects retain unknown fields. No operation connects, reads a clock, or writes
//! a camera until an application passes its request to a transport.

use std::fmt;

use crate::document::Node;
use crate::error::Kind;
use crate::{Document, Error, Method, Request};

mod audio;
mod capabilities;
mod configuration;
mod hosts;
mod ptz;
mod rules;
mod status;

#[doc(inline)]
pub use audio::{AudioChannel, AudioCodec, AudioSession};
pub use capabilities::{Capabilities, Endpoint, FieldCapability};
pub use configuration::{DeviceInfo, Motion, Stream, Time, TimeMode};
pub use hosts::CallbackHost;
pub use ptz::{Preset, PtzCapabilities, PtzStatus};
pub use rules::{Rule, RuleKind};
pub use status::ResponseStatus;

/// A read-only request bound to one validated response type.
pub struct Query<Output> {
    request: Request,
    parse: fn(Document) -> Result<Output, Error>,
}

impl<Output> Query<Output> {
    fn for_request(request: Request, parse: fn(Document) -> Result<Output, Error>) -> Self {
        Self { request, parse }
    }
    fn new(
        resource: impl AsRef<str>,
        parse: fn(Document) -> Result<Output, Error>,
    ) -> Result<Self, Error> {
        Ok(Self {
            request: Request::get(resource)?,
            parse,
        })
    }
    /// Returns the exact request used by any Sans-I/O-compatible transport.
    pub const fn request(&self) -> &Request {
        &self.request
    }
    /// Decodes the media type, status and expected response shape.
    ///
    /// # Errors
    /// Rejects malformed, oversized, failed or wrong-resource responses.
    pub fn parse(&self, content_type: &str, bytes: &[u8]) -> Result<Output, Error> {
        let document = Document::parse(content_type, bytes)?;
        if let Some(status) = ResponseStatus::from_document(&document)? {
            status.ensure_success()?;
            return Err(Error::new(Kind::Protocol));
        }
        (self.parse)(document)
    }
}

impl<Output> fmt::Debug for Query<Output> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Query")
            .field("request", &self.request)
            .finish_non_exhaustive()
    }
}

const fn nonzero(value: u32) -> Result<u32, Error> {
    if value == 0 {
        Err(Error::new(Kind::InvalidInput))
    } else {
        Ok(value)
    }
}

fn required(node: Node<'_>, name: &str) -> Result<String, Error> {
    node.field(name)?
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Error::new(Kind::Protocol))
}

fn number(node: Node<'_>, name: &str) -> Result<Option<u32>, Error> {
    node.field(name)?
        .map(|value| value.parse().map_err(|_| Error::new(Kind::Protocol)))
        .transpose()
}

fn boolean(node: Node<'_>, name: &str) -> Result<bool, Error> {
    match required(node, name)?.as_str() {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => Err(Error::new(Kind::Protocol)),
    }
}

fn replacement(document: &Document, resource: String) -> Result<Request, Error> {
    Request::with_body(
        Method::Put,
        resource,
        document.format(),
        document.to_bytes()?,
    )
}
