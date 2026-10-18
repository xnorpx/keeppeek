//! Verified fourth-tier replicas, separate from authoritative local volume moves.

pub(crate) mod filesystem;
mod operations;
mod playback;
pub mod settings;
pub(crate) mod target;
mod worker;

pub(crate) use playback::{VerifiedReader, read_range};
pub(crate) use worker::{Handle, Worker};

// A range read verifies at most two extra chunks without reading the whole recording.
pub(crate) const CHUNK_BYTES: u64 = 4 * 1_048_576;
pub(crate) const PROBE_PAYLOAD: &[u8] = b"KeepPeek archive ownership probe\n";
