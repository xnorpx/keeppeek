//! MP4-backed RTSP server support.
//!
//! [`RtspServer`] serves one or more H.264 or H.265 MP4 profiles through RTSP.
//! It accepts TCP-interleaved and UDP-unicast RTP setup requests. Each profile
//! replays its MP4 samples on every client connection, making the server useful
//! for local integration environments and deterministic camera simulations.
//!
//! # Examples
//!
//! ```no_run
//! use retina::server::RtspServer;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let server = RtspServer::from_mp4_streams_on(
//!     "127.0.0.1:8554".parse()?,
//!     "main.mp4",
//!     "sub.mp4",
//! )?;
//! println!("main profile: {}", server.high_resolution_url());
//! println!("sub profile: {}", server.low_resolution_url());
//! # Ok(())
//! # }
//! ```

#[doc(inline)]
pub use crate::testutil::fake_camera::{
    FakeRtspCamera as RtspServer, FakeRtspCameraError as RtspServerError,
    FakeRtspCameraTranscript as RtspServerTranscript,
};
