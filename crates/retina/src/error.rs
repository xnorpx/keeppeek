// Copyright (C) The Retina Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::{ConnectionContext, PacketContext, StreamContext};
use std::sync::Arc;

/// An opaque `std::error::Error + Send + Sync + 'static` implementation.
///
/// Currently the focus is on providing detailed human-readable error messages.
/// In most cases they have enough information to find the offending packet
/// in Wireshark.
///
/// If you wish to inspect Retina errors programmatically, or if you need
/// errors formatted in a different way, please file an issue on the `retina`
/// repository.
#[derive(Clone, derive_more::Debug, derive_more::Display, derive_more::Error)]
#[display("{_0}")]
#[debug("{_0:?}")]
pub struct Error(#[error(not(source))] pub(crate) Arc<ErrorInt>);

#[derive(Debug, derive_more::Display, derive_more::Error)]
pub enum ErrorInt {
    /// The method's caller provided an invalid argument.
    #[display("Invalid argument: {_0}")]
    InvalidArgument(#[error(not(source))] String),

    #[display("{description}\n\nconn: {conn_ctx}\nstream: {stream_ctx}\npkt: {pkt_ctx}")]
    PacketError {
        conn_ctx: ConnectionContext,
        stream_ctx: StreamContext,
        pkt_ctx: PacketContext,
        stream_id: usize,
        description: String,
    },

    #[display(
        "{description}\n\n\
             conn: {conn_ctx}\nstream: {stream_ctx}\n\
             ssrc: {ssrc:08x}\nseq: {sequence_number}\npkt: {pkt_ctx}"
    )]
    RtpPacketError {
        conn_ctx: ConnectionContext,
        stream_ctx: StreamContext,
        pkt_ctx: crate::PacketContext,
        stream_id: usize,
        ssrc: u32,
        sequence_number: u16,
        description: String,
    },

    #[display("Unsupported: {_0}")]
    Unsupported(#[error(not(source))] String),
}
