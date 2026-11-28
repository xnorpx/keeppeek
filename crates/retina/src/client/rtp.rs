// Copyright (C) The Retina Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! RTP and RTCP handling; see [RFC 3550](https://datatracker.ietf.org/doc/html/rfc3550).

use super::{SessionOptions, Timeline, UnknownRtcpSsrcPolicy};
use crate::{
    ConnectionContext, Error, PacketContext, PacketContextInner, StreamContext, StreamContextInner,
    client::PacketItem,
    error::ErrorInt,
    rtcp::ReceivedCompoundPacket,
    rtp::{RawPacket, ReceivedPacket},
};
use bytes::Bytes;
use log::{debug, log_enabled, trace, warn};

/// Describes how Retina formed its initial expectation for the stream's `ssrc` or `seq`.
#[derive(Copy, Clone, Debug)]
enum InitialExpectation {
    PlayResponseHeader,
    RtpPacket,
    RtcpPacket,
}

#[derive(Copy, Clone)]
struct Ssrc {
    init: InitialExpectation,
    ssrc: u32,
}

impl std::fmt::Debug for Ssrc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        f.debug_struct("Ssrc")
            .field("init", &self.init)
            .field("ssrc", &format_args!("0x{:08x}", self.ssrc))
            .finish()
    }
}

#[derive(Copy, Clone, Debug)]
struct Seq {
    init: InitialExpectation,
    next: u16,
}

/// RTP/RTCP demarshaller which ensures packets have the correct SSRC and
/// monotonically increasing SEQ. Unstable; exposed for benchmark.
///
/// When using UDP, skips and logs out-of-order packets. When using TCP,
/// fails on them.
///
/// This reports packet loss (via [ReceivedPacket::loss]) but doesn't prohibit it
/// of more than `i16::MAX` which would be indistinguishable from non-monotonic sequence numbers.
/// Servers sometimes drop packets internally even when sending data via TCP.
///
/// At least [one camera](https://github.com/scottlamb/moonfire-nvr/wiki/Cameras:-Reolink#reolink-rlc-410-hardware-version-ipc_3816m)
/// sometimes sends data from old RTSP sessions over new ones. This seems like a
/// serious bug, and currently `InorderRtpParser` will error in this case,
/// although it'd be possible to discard the incorrect SSRC instead.
///
/// [RFC 3550 section 8.2](https://tools.ietf.org/html/rfc3550#section-8.2) says that SSRC
/// can change mid-session with a RTCP BYE message. This currently isn't handled. I'm
/// not sure it will ever come up with IP cameras.
#[doc(hidden)] // pub only for the benchmarks; not a stable API.
#[derive(Debug)]
pub struct InorderParser {
    ssrc: Option<Ssrc>,
    seq: Option<Seq>,

    /// The stream's RTP payload type from the SDP description.
    payload_type: u8,

    /// Total RTP packets seen in this stream.
    seen_rtp_packets: u64,
    clock: Option<PacketClock>,
    sender_report: Option<(u32, crate::NtpTimestamp)>,

    /// Total RTCP packets seen in this stream.
    seen_rtcp_packets: u64,

    unknown_rtcp_session: UnknownRtcpSsrcPolicy,
    seen_unknown_rtcp_session: bool,

    /// Tracks whether this parser already logged a packet from another stream.
    seen_other_stream: bool,
}

/// Source clock readings from accepted RTP packets in one RTSP PLAY session.
#[derive(Clone, Copy, Debug)]
pub struct PacketClock {
    pub first: crate::Timestamp,
    pub latest: crate::Timestamp,
    pub backward_steps: u64,
    pub missing_sequence_numbers: u64,
}

fn note_stale_live555_data_if_tcp(
    _tool: Option<&super::Tool>,
    _session_options: &SessionOptions,
    _conn_ctx: &crate::ConnectionContext,
    stream_ctx: &StreamContext,
    pkt_ctx: &PacketContext,
) {
    if let (
        StreamContext(StreamContextInner::Tcp(stream_ctx)),
        PacketContext(PacketContextInner::Tcp { msg_ctx }),
    ) = (stream_ctx, pkt_ctx)
    {
        warn!(
            "received interleaved media from an unexpected SSRC on channel {} at {}",
            stream_ctx.rtp_channel_id, msg_ctx
        );
    }
}

impl InorderParser {
    pub fn new(
        ssrc: Option<u32>,
        next_seq: Option<u16>,
        payload_type: u8,
        unknown_rtcp_session: UnknownRtcpSsrcPolicy,
    ) -> Self {
        Self {
            ssrc: ssrc.map(|ssrc| Ssrc {
                init: InitialExpectation::PlayResponseHeader,
                ssrc,
            }),
            seq: next_seq.map(|next| Seq {
                init: InitialExpectation::PlayResponseHeader,
                next,
            }),
            payload_type,
            seen_rtp_packets: 0,
            clock: None,
            sender_report: None,
            seen_rtcp_packets: 0,
            unknown_rtcp_session,
            seen_unknown_rtcp_session: false,
            seen_other_stream: false,
        }
    }

    /// Returns true if `raw` belongs to another stream sent on this channel.
    ///
    /// Some cameras send unsubscribed audio and its RTCP packets on the video
    /// stream's interleaved channel. Stale data from a prior session still uses
    /// this stream's payload type and remains an error.
    pub(crate) fn is_other_stream(&self, raw: &RawPacket) -> bool {
        raw.payload_type() != self.payload_type
            && !matches!(self.ssrc, Some(s) if s.ssrc == raw.ssrc())
    }

    pub(super) fn clock_snapshot(
        &self,
        play_origin: Option<u32>,
    ) -> Option<super::RtpClockSnapshot> {
        Some(super::RtpClockSnapshot {
            packets: self.seen_rtp_packets,
            clock: self.clock?,
            sender_report: self.sender_report,
            play_origin,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn rtp(
        &mut self,
        session_options: &SessionOptions,
        stream_ctx: &StreamContext,
        tool: Option<&super::Tool>,
        conn_ctx: &ConnectionContext,
        pkt_ctx: &PacketContext,
        timeline: &mut Timeline,
        stream_id: usize,
        data: Bytes,
    ) -> Result<Option<PacketItem>, Error> {
        let (raw, payload_range) = RawPacket::new(data).map_err(|e| {
            wrap!(ErrorInt::PacketError {
                conn_ctx: *conn_ctx,
                stream_ctx: stream_ctx.to_owned(),
                pkt_ctx: *pkt_ctx,
                stream_id,
                description: format!(
                    "corrupt RTP header while expecting seq={:?}: {:?}\n{:#?}",
                    self.seq,
                    e.reason,
                    crate::hex::LimitedHex::new(&e.data[..], 64),
                ),
            })
        })?;

        // Skip pt=50 packets, sent by at least Geovision cameras. I'm not sure
        // what purpose these serve, but they have the same sequence number as
        // the packet immediately before. In TCP streams, this can cause an
        // "Out-of-order packet or large loss" error. In UDP streams, if these
        // are delivered out of order, they will cause the more important other
        // packet with the same sequence number to be skipped.
        if raw.payload_type() == 50 {
            debug!("skipping pkt with invalid payload type 50");
            return Ok(None);
        }

        if self.is_other_stream(&raw) {
            if !self.seen_other_stream {
                warn!(
                    "skipping RTP packets of another stream on stream {stream_id}'s channel: \
                     ssrc={:08x} pt={} while expecting ssrc={:08x?} pt={}; won't log further ones",
                    raw.ssrc(),
                    raw.payload_type(),
                    self.ssrc.map(|s| s.ssrc),
                    self.payload_type,
                );
                self.seen_other_stream = true;
            }
            return Ok(None);
        }

        let sequence_number = raw.sequence_number();
        let ssrc = raw.ssrc();
        let loss =
            sequence_number.wrapping_sub(self.seq.map(|s| s.next).unwrap_or(sequence_number));
        if matches!(self.ssrc, Some(s) if s.ssrc != ssrc) {
            debug!(
                "rtp unexpected ssrc: stream_id={stream_id} got=0x{ssrc:08x} expected={:?} seq={sequence_number}",
                self.ssrc
            );
            note_stale_live555_data_if_tcp(tool, session_options, conn_ctx, stream_ctx, pkt_ctx);
            bail!(ErrorInt::RtpPacketError {
                conn_ctx: *conn_ctx,
                pkt_ctx: *pkt_ctx,
                stream_ctx: stream_ctx.to_owned(),
                stream_id,
                ssrc,
                sequence_number,
                description: format!(
                    "wrong ssrc after {} RTP pkts + {} RTCP pkts; expecting ssrc={:?} seq={:?}",
                    self.seen_rtp_packets, self.seen_rtcp_packets, self.ssrc, self.seq,
                ),
            });
        } else if self.ssrc.is_none() {
            debug!(
                "rtp ssrc bound: stream_id={stream_id} ssrc=0x{ssrc:08x} pt={} seq={sequence_number}",
                raw.payload_type()
            );
            self.ssrc = Some(Ssrc {
                init: InitialExpectation::RtpPacket,
                ssrc,
            });
        }
        if loss > 0x80_00 {
            debug!(
                "rtp ooo/gap: stream_id={stream_id} ssrc=0x{ssrc:08x} seq={sequence_number} expected={:?} gap={loss}",
                self.seq.map(|s| s.next)
            );
            bail!(ErrorInt::RtpPacketError {
                conn_ctx: *conn_ctx,
                pkt_ctx: *pkt_ctx,
                stream_ctx: stream_ctx.to_owned(),
                stream_id,
                ssrc,
                sequence_number,
                description: format!(
                    "Out-of-order packet or large loss; expecting ssrc={:08x?} seq={:?}",
                    self.ssrc, self.seq
                ),
            });
        }
        if loss > 0 {
            debug!(
                "rtp seq gap: stream_id={stream_id} ssrc=0x{ssrc:08x} seq={sequence_number} expected={:?} gap={loss}",
                self.seq.map(|s| s.next)
            );
        }
        let timestamp = match timeline.advance_to(raw.timestamp()) {
            Ok(ts) => ts,
            Err(description) => {
                debug!(
                    "rtp timeline reject: stream_id={stream_id} ssrc=0x{ssrc:08x} seq={sequence_number} reason={description}"
                );
                bail!(ErrorInt::RtpPacketError {
                    conn_ctx: *conn_ctx,
                    pkt_ctx: *pkt_ctx,
                    stream_ctx: stream_ctx.to_owned(),
                    stream_id,
                    ssrc,
                    sequence_number,
                    description,
                });
            }
        };
        if log_enabled!(log::Level::Trace) {
            trace!(
                "rtp pkt: stream_id={stream_id} ssrc=0x{ssrc:08x} pt={} seq={sequence_number} ts={} rtp_ts={} mark={} len={} loss={loss}",
                raw.payload_type(),
                timestamp.timestamp(),
                raw.timestamp(),
                raw.mark(),
                payload_range.len()
            );
        }
        self.seq = Some(Seq {
            init: self
                .seq
                .map(|s| s.init)
                .unwrap_or(InitialExpectation::RtpPacket),
            next: sequence_number.wrapping_add(1),
        });
        self.seen_rtp_packets += 1;
        let clock = self.clock.get_or_insert(PacketClock {
            first: timestamp,
            latest: timestamp,
            backward_steps: 0,
            missing_sequence_numbers: 0,
        });
        clock.backward_steps = clock
            .backward_steps
            .saturating_add(u64::from(timestamp.timestamp() < clock.latest.timestamp()));
        clock.missing_sequence_numbers = clock
            .missing_sequence_numbers
            .saturating_add(u64::from(loss));
        clock.latest = timestamp;
        Ok(Some(PacketItem::Rtp(ReceivedPacket {
            ctx: *pkt_ctx,
            stream_id,
            timestamp,
            raw,
            payload_range,
            loss,
        })))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn rtcp(
        &mut self,
        session_options: &SessionOptions,
        stream_ctx: &StreamContext,
        tool: Option<&super::Tool>,
        conn_ctx: &ConnectionContext,
        pkt_ctx: &PacketContext,
        timeline: &mut Timeline,
        stream_id: usize,
        data: Bytes,
    ) -> Result<Option<PacketItem>, String> {
        let first_pkt = crate::rtcp::ReceivedCompoundPacket::validate(&data[..])?;
        let mut rtp_timestamp = None;
        if let Ok(Some(sr)) = first_pkt.as_sender_report() {
            // Check the SSRC before placing the timestamp so a dropped sender
            // report cannot establish the stream NPT epoch.
            let ssrc = sr.ssrc();
            if matches!(self.ssrc, Some(s) if s.ssrc != ssrc) {
                match self.unknown_rtcp_session {
                    UnknownRtcpSsrcPolicy::AbortSession => {
                        note_stale_live555_data_if_tcp(
                            tool,
                            session_options,
                            conn_ctx,
                            stream_ctx,
                            pkt_ctx,
                        );
                        return Err(format!(
                            "Expected ssrc={:08x?}, got RTCP SR ssrc={:08x}",
                            self.ssrc, ssrc
                        ));
                    }
                    UnknownRtcpSsrcPolicy::Default | UnknownRtcpSsrcPolicy::DropPackets => {
                        if !self.seen_unknown_rtcp_session {
                            warn!(
                                "saw unknown rtcp ssrc {ssrc}; rtp session has ssrc {s:?}",
                                s = self.ssrc
                            );
                            self.seen_unknown_rtcp_session = true;
                        }
                        return Ok(None);
                    }
                    UnknownRtcpSsrcPolicy::ProcessPackets => {}
                }
            } else if self.ssrc.is_none()
                && !matches!(
                    self.unknown_rtcp_session,
                    UnknownRtcpSsrcPolicy::ProcessPackets
                )
            {
                self.ssrc = Some(Ssrc {
                    init: InitialExpectation::RtcpPacket,
                    ssrc,
                });
            }
            rtp_timestamp = Some(timeline.place(sr.rtp_timestamp()).map_err(
                |mut description| {
                    description.push_str(" in RTCP SR");
                    description
                },
            )?);
            // ProcessPackets can deliver other senders' control packets to the caller.
            if self.ssrc.is_some_and(|source| source.ssrc == ssrc) {
                let first_sr = self.sender_report.is_none();
                self.sender_report =
                    (sr.ntp_timestamp().0 != 0).then(|| (sr.rtp_timestamp(), sr.ntp_timestamp()));
                if first_sr && self.sender_report.is_some() {
                    debug!(
                        "rtcp sender report initialized: stream_id={stream_id} ssrc=0x{ssrc:08x} rtp_ts={} ntp=0x{:016x}",
                        sr.rtp_timestamp(),
                        sr.ntp_timestamp().0
                    );
                }
            }
        }
        self.seen_rtcp_packets += 1;
        let compound = ReceivedCompoundPacket {
            ctx: *pkt_ctx,
            stream_id,
            rtp_timestamp,
            raw: data,
        };
        if log_enabled!(log::Level::Trace) {
            log_rtcp_compound_trace(stream_id, &compound);
        }
        Ok(Some(PacketItem::Rtcp(compound)))
    }
}

fn log_rtcp_compound_trace(stream_id: usize, compound: &ReceivedCompoundPacket) {
    for pkt in compound.pkts() {
        match pkt.as_typed() {
            Ok(Some(crate::rtcp::TypedPacketRef::SenderReport(sr))) => {
                trace!(
                    "rtcp SR: stream_id={stream_id} ssrc=0x{:08x} ntp=0x{:016x} rtp_ts={} blocks={}",
                    sr.ssrc(),
                    sr.ntp_timestamp().0,
                    sr.rtp_timestamp(),
                    sr.count()
                );
            }
            Ok(Some(crate::rtcp::TypedPacketRef::ReceiverReport(rr))) => {
                trace!(
                    "rtcp RR: stream_id={stream_id} ssrc=0x{:08x} blocks={}",
                    rr.ssrc(),
                    rr.count()
                );
            }
            Ok(None) => {
                trace!(
                    "rtcp packet: stream_id={stream_id} pt={} count={} len={}",
                    pkt.payload_type(),
                    pkt.count(),
                    pkt.raw().len()
                );
            }
            Err(error) => {
                debug!(
                    "rtcp subpacket invalid: stream_id={stream_id} pt={} err={error}",
                    pkt.payload_type()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    mod clock;
    mod timing_benchmark;
    use super::*;
    use std::time::Duration;

    /// Checks dropping and logging Geovision's extra payload type 50 packets.
    /// On a GV-EBD4701 running V1.02_2021_04_08, these seem to appear after
    /// every IDR frame, with the same sequence number as the final packet in
    /// that frame.
    #[test]
    fn geovision_pt50_packet() {
        let mut timeline = Timeline::new(None, 90_000, None).unwrap();
        let mut parser =
            InorderParser::new(Some(0xd25614e), None, 105, UnknownRtcpSsrcPolicy::Default);
        let stream_ctx = StreamContext::dummy();

        // Normal packet.
        let (pkt, _payload_range) = crate::rtp::RawPacketBuilder {
            sequence_number: 0x1234,
            timestamp: 141000,
            payload_type: 105,
            ssrc: 0xd25614e,
            mark: true,
        }
        .build(*b"foo")
        .unwrap();
        match parser.rtp(
            &SessionOptions::default(),
            &stream_ctx,
            None,
            &ConnectionContext::dummy(),
            &PacketContext::dummy(),
            &mut timeline,
            0,
            pkt.0,
        ) {
            Ok(Some(PacketItem::Rtp(_))) => {}
            o => panic!("unexpected packet 1 result: {o:#?}"),
        }

        // Mystery pt=50 packet with same sequence number.
        let (pkt, _payload_range) = crate::rtp::RawPacketBuilder {
            sequence_number: 0x1234,
            timestamp: 141000,
            payload_type: 50,
            ssrc: 0xd25614e,
            mark: true,
        }
        .build(*b"bar")
        .unwrap();
        match parser.rtp(
            &SessionOptions::default(),
            &stream_ctx,
            None,
            &ConnectionContext::dummy(),
            &PacketContext::dummy(),
            &mut timeline,
            0,
            pkt.0,
        ) {
            Ok(None) => {}
            o => panic!("unexpected packet 2 result: {o:#?}"),
        }
    }

    #[test]
    fn out_of_order_tcp_packet_fails() {
        let mut timeline = Timeline::new(None, 90_000, None).unwrap();
        let mut parser =
            InorderParser::new(Some(0xd25614e), None, 96, UnknownRtcpSsrcPolicy::Default);
        let stream_ctx = StreamContext::dummy();
        let session_options = SessionOptions::default();
        let (pkt, _payload_range) = crate::rtp::RawPacketBuilder {
            sequence_number: 2,
            timestamp: 2,
            payload_type: 96,
            ssrc: 0xd25614e,
            mark: true,
        }
        .build(*b"pkt 2")
        .unwrap();
        assert!(matches!(
            parser.rtp(
                &session_options,
                &stream_ctx,
                None,
                &ConnectionContext::dummy(),
                &PacketContext::dummy(),
                &mut timeline,
                0,
                pkt.0,
            ),
            Ok(Some(PacketItem::Rtp(_)))
        ));

        let (pkt, _payload_range) = crate::rtp::RawPacketBuilder {
            sequence_number: 1,
            timestamp: 1,
            payload_type: 96,
            ssrc: 0xd25614e,
            mark: true,
        }
        .build(*b"pkt 1")
        .unwrap();
        assert!(
            parser
                .rtp(
                    &session_options,
                    &stream_ctx,
                    None,
                    &ConnectionContext::dummy(),
                    &PacketContext::dummy(),
                    &mut timeline,
                    0,
                    pkt.0,
                )
                .is_err()
        );
    }

    #[test]
    fn sequence_and_timestamp_rollovers_are_unwrapped_together() {
        let mut timeline = Timeline::new(None, 90_000, None).unwrap();
        let mut parser =
            InorderParser::new(Some(0xd25614e), None, 96, UnknownRtcpSsrcPolicy::Default);
        let stream_ctx = StreamContext::dummy();
        let session_options = SessionOptions::default();

        let (first, _) = crate::rtp::RawPacketBuilder {
            sequence_number: u16::MAX,
            timestamp: u32::MAX - 2_999,
            payload_type: 96,
            ssrc: 0xd25614e,
            mark: true,
        }
        .build(*b"first")
        .unwrap();
        let Some(PacketItem::Rtp(first)) = parser
            .rtp(
                &session_options,
                &stream_ctx,
                None,
                &ConnectionContext::dummy(),
                &PacketContext::dummy(),
                &mut timeline,
                0,
                first.0,
            )
            .unwrap()
        else {
            panic!("first RTP packet was not emitted");
        };
        assert_eq!(first.timestamp().elapsed_duration(), Some(Duration::ZERO));

        let (second, _) = crate::rtp::RawPacketBuilder {
            sequence_number: 0,
            timestamp: 0,
            payload_type: 96,
            ssrc: 0xd25614e,
            mark: true,
        }
        .build(*b"second")
        .unwrap();
        let Some(PacketItem::Rtp(second)) = parser
            .rtp(
                &session_options,
                &stream_ctx,
                None,
                &ConnectionContext::dummy(),
                &PacketContext::dummy(),
                &mut timeline,
                0,
                second.0,
            )
            .unwrap()
        else {
            panic!("second RTP packet was not emitted");
        };
        assert_eq!(
            second.timestamp().elapsed_duration(),
            Some(Duration::new(0, 33_333_333))
        );
    }

    fn push_rtp(
        parser: &mut InorderParser,
        timeline: &mut Timeline,
        seq: u16,
        payload_type: u8,
        ssrc: u32,
    ) -> Result<bool, Error> {
        let (pkt, _) = crate::rtp::RawPacketBuilder {
            sequence_number: seq,
            timestamp: u32::from(seq) * 3000,
            payload_type,
            ssrc,
            mark: true,
        }
        .build(*b"pkt")
        .unwrap();
        parser
            .rtp(
                &SessionOptions::default(),
                &StreamContext::dummy(),
                None,
                &ConnectionContext::dummy(),
                &PacketContext::dummy(),
                timeline,
                0,
                pkt.0,
            )
            .map(|packet| packet.is_some())
    }

    #[test]
    fn other_stream_on_the_channel() {
        let mut timeline = Timeline::new(None, 90_000, None).unwrap();
        let mut parser = InorderParser::new(None, None, 96, UnknownRtcpSsrcPolicy::Default);

        assert!(!push_rtp(&mut parser, &mut timeline, 23161, 8, 0x111).unwrap());
        assert!(!push_rtp(&mut parser, &mut timeline, 0x1c, 72, 0xec8e_2b37).unwrap());
        assert!(push_rtp(&mut parser, &mut timeline, 1, 96, 0x110).unwrap());
        assert!(!push_rtp(&mut parser, &mut timeline, 23162, 8, 0x111).unwrap());
        assert!(push_rtp(&mut parser, &mut timeline, 2, 96, 0x110).unwrap());
        assert!(push_rtp(&mut parser, &mut timeline, 3, 96, 0x112).is_err());
    }

    #[test]
    fn stray_sender_report() {
        let mut timeline = Timeline::new(None, 90_000, None).unwrap();
        let mut parser = InorderParser::new(Some(0x110), None, 96, UnknownRtcpSsrcPolicy::Default);
        let mut sr = [0_u8; 28];
        sr[..4].copy_from_slice(&[0x80, 200, 0, 6]);
        sr[4..8].copy_from_slice(&0x111_u32.to_be_bytes());
        sr[16..20].copy_from_slice(&1_000_000_u32.to_be_bytes());
        let item = parser
            .rtcp(
                &SessionOptions::default(),
                &StreamContext::dummy(),
                None,
                &ConnectionContext::dummy(),
                &PacketContext::dummy(),
                &mut timeline,
                0,
                Bytes::copy_from_slice(&sr),
            )
            .unwrap();
        assert!(item.is_none());
        assert_eq!(timeline.advance_to(3_000).unwrap().elapsed(), 0);
    }
}
