//! In-process camera protocol capture, bitstream analysis, and repro-fixture export.
//!
//! This module captures decrypted camera protocol traffic into PCAPNG files,
//! extracts raw video and audio elementary bitstreams alongside a muxed MP4
//! copy, detects common RTP and H.26x anomalies, stores captures in a bounded
//! local directory, and exports sanitized `keeppeek.camera-repro.v1` fixtures.

pub mod bitstream;
pub mod pcapng;
pub mod protocols;
pub mod repro;
pub mod store;
pub mod traces;
