#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]
#![warn(missing_docs, missing_debug_implementations)]

mod analytics;
mod auth;
mod document;
mod encoding;
mod error;
mod event;
mod images;
mod multipart;
mod request;

pub mod management;

#[cfg(feature = "ureq")]
pub mod blocking;

#[doc(inline)]
pub use analytics::{BoundingBox, Confidence, CoordinateSpace, ImageRef, Object, Target};
#[doc(inline)]
pub use auth::{Authorization, CallbackAuth, Credentials, Session};
#[doc(inline)]
pub use document::{Document, Element};
#[doc(inline)]
pub use error::Error;
#[doc(inline)]
pub use event::Event;
#[doc(inline)]
pub use images::{Assembler, Bundle, Image};
#[doc(inline)]
pub use multipart::{Decoder, Part, PartKind};
#[doc(inline)]
pub use request::{Format, Method, Ptz, Request};

/// Maximum encoded XML request or response size, excluding transport headers.
pub const XML_SIZE_BYTES_MAX: usize = 256 * 1024;
