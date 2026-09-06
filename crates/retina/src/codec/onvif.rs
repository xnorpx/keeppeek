// Copyright (C) The Retina Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! ONVIF metadata streams.
//!
//! See the
//! [ONVIF Streaming Specification](https://www.onvif.org/specs/stream/ONVIF-Streaming-Spec.pdf)
//! version 19.12 section 5.2.1.1. The RTP layer muxing is simple: RTP packets with the MARK
//! bit set end messages.
//!
//! Assembly accepts at most 256 KiB of raw or compressed input and 1,024 packets per
//! document. A document must finish within 10 seconds of its first packet's receive time.
//! Deadlines are checked when another packet arrives. Rejected documents are discarded
//! through the next marker so that a damaged suffix cannot become a complete message.
//! Single-packet messages copy their payload so that they do not retain a larger transport buffer.
//! Payload decoding and validation belong to the consumer.

use super::{CodecItem, MessageParameters};
use crate::codec::DepacketizeError;
use std::time::{Duration, Instant};

/// Limits metadata memory independently of the video and audio streams.
const DOCUMENT_BYTES_MAX: usize = 256 * 1024;
/// Avoids reserving a rare large document's allocation for every subsequent message.
const HIGH_WATER_BYTES_MAX: usize = 16 * 1024;
/// Bounds assembly work even when fragments contain no payload.
const DOCUMENT_PACKETS_MAX: u16 = 1024;
/// Limits how long a slow sender can extend one document when packets keep arriving.
const ASSEMBLY_DURATION_MAX: Duration = Duration::from_secs(10);

/// The advertised encoding of an ONVIF metadata payload.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum CompressionType {
    /// XML without compression.
    Uncompressed,
    /// XML compressed with gzip. The consumer must decompress the payload.
    GzipCompressed,
    /// EXI with the ONVIF default schema. Retina does not decode EXI.
    ExiDefault,
    /// EXI with in-band schema information. Retina does not decode EXI.
    ExiInBand,
}

pub(super) fn compression_type(encoding_name: &str) -> Option<CompressionType> {
    match encoding_name {
        "vnd.onvif.metadata" => Some(CompressionType::Uncompressed),
        "vnd.onvif.metadata+gzip" | "vnd.onvif.metadata.gzip" => {
            Some(CompressionType::GzipCompressed)
        }
        "vnd.onvif.metadata.exi.onvif" => Some(CompressionType::ExiDefault),
        "vnd.onvif.metadata.exi.ext" => Some(CompressionType::ExiInBand),
        _ => None,
    }
}

#[derive(Debug)]
pub struct Depacketizer {
    parameters: MessageParameters,
    state: State,
    high_water_size: usize,
    loss: u16,
}

#[derive(Debug)]
enum State {
    Idle,
    InProgress(InProgress),
    Discarding,
    Ready(super::MessageFrame),
}

#[derive(Debug)]
struct InProgress {
    ctx: crate::PacketContext,
    timestamp: crate::Timestamp,
    received: Instant,
    data: Vec<u8>,
    packet_count: u16,
}

impl InProgress {
    fn discard_reason(&self, pkt: &crate::rtp::ReceivedPacket) -> Option<&'static str> {
        if self.timestamp.timestamp != pkt.timestamp().timestamp {
            return Some("RTP timestamp changed");
        }
        let elapsed = pkt.ctx().received().checked_duration_since(self.received);
        if elapsed.is_none_or(|elapsed| elapsed >= ASSEMBLY_DURATION_MAX) {
            return Some("invalid or expired receive deadline");
        }
        if self.packet_count >= DOCUMENT_PACKETS_MAX - 1 && !pkt.mark() {
            return Some("packet limit reached without a marker");
        }
        if pkt.payload().len() > DOCUMENT_BYTES_MAX - self.data.len() {
            return Some("document byte limit exceeded");
        }
        if pkt.mark() && self.data.is_empty() && pkt.payload().is_empty() {
            return Some("empty document");
        }
        None
    }

    fn append(&mut self, payload: &[u8]) {
        assert!(
            self.packet_count < DOCUMENT_PACKETS_MAX,
            "metadata packet limit exceeded"
        );
        let length = self.data.len() + payload.len();
        assert!(length <= DOCUMENT_BYTES_MAX, "metadata byte limit exceeded");
        if length > self.data.capacity() {
            let capacity = (self.data.capacity() * 2)
                .max(length)
                .min(DOCUMENT_BYTES_MAX);
            self.data.reserve_exact(capacity - self.data.len());
        }
        self.data.extend_from_slice(payload);
        self.packet_count += 1;
    }
}

impl Depacketizer {
    pub(super) const fn new(compression_type: CompressionType) -> Self {
        Self {
            parameters: MessageParameters(compression_type),
            state: State::Idle,
            high_water_size: 0,
            loss: 0,
        }
    }

    pub(super) const fn parameters(&self) -> Option<super::ParametersRef<'_>> {
        Some(super::ParametersRef::Message(&self.parameters))
    }

    pub(super) fn push(&mut self, pkt: crate::rtp::ReceivedPacket) -> Result<(), String> {
        assert!(
            !matches!(self.state, State::Ready(_)),
            "push while a metadata message is ready"
        );
        self.loss = self.loss.saturating_add(pkt.loss());
        if matches!(self.state, State::Discarding) {
            if pkt.mark() {
                self.state = State::Idle;
            }
        } else if pkt.loss() > 0 {
            self.discard(pkt.mark(), "RTP packet loss");
        } else {
            self.push_fragment(pkt);
        }
        Ok(())
    }

    fn discard(&mut self, mark: bool, reason: &'static str) {
        log::debug!("Discarding ONVIF metadata document: {reason}");
        self.state = if mark { State::Idle } else { State::Discarding };
    }

    fn push_fragment(&mut self, pkt: crate::rtp::ReceivedPacket) {
        let mut in_progress = match std::mem::replace(&mut self.state, State::Idle) {
            State::InProgress(in_progress) => in_progress,
            State::Ready(_) | State::Discarding => unreachable!("invalid assembly state"),
            State::Idle => {
                if pkt.mark() {
                    if pkt.payload().is_empty() {
                        self.discard(true, "empty document");
                        return;
                    }
                    self.state = State::Ready(super::MessageFrame {
                        stream_id: pkt.stream_id(),
                        loss: std::mem::take(&mut self.loss),
                        ctx: *pkt.ctx(),
                        timestamp: pkt.timestamp(),
                        data: bytes::Bytes::copy_from_slice(pkt.payload()),
                    });
                    return;
                }
                InProgress {
                    ctx: *pkt.ctx(),
                    timestamp: pkt.timestamp(),
                    received: pkt.ctx().received(),
                    data: Vec::with_capacity(self.high_water_size),
                    packet_count: 0,
                }
            }
        };
        if let Some(reason) = in_progress.discard_reason(&pkt) {
            self.discard(pkt.mark(), reason);
            return;
        }
        in_progress.append(pkt.payload());
        if pkt.mark() {
            self.high_water_size = self
                .high_water_size
                .max(in_progress.data.len().min(HIGH_WATER_BYTES_MAX));
            self.state = State::Ready(super::MessageFrame {
                stream_id: pkt.stream_id(),
                ctx: in_progress.ctx,
                timestamp: in_progress.timestamp,
                data: in_progress.data.into(),
                loss: std::mem::take(&mut self.loss),
            });
        } else {
            self.state = State::InProgress(in_progress);
        }
    }

    pub(super) fn pull(&mut self) -> Option<Result<CodecItem, DepacketizeError>> {
        match std::mem::replace(&mut self.state, State::Idle) {
            State::Ready(message) => Some(Ok(CodecItem::MessageFrame(message))),
            s => {
                self.state = s;
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CodecItem, CompressionType, Depacketizer, State};
    use crate::codec::MessageFrame;
    use crate::rtp::{ReceivedPacket, ReceivedPacketBuilder};
    use std::num::NonZeroU32;
    use std::time::{Duration, Instant, UNIX_EPOCH};

    const DOCUMENT: &[u8] = b"<Event><State>true</State></Event>";
    const RECOVERED: &[u8] = b"<Event><State>false</State></Event>";

    fn packet(sequence_number: u16, mark: bool, payload: &[u8]) -> ReceivedPacket {
        ReceivedPacketBuilder {
            ctx: crate::PacketContext::dummy(),
            stream_id: 2,
            sequence_number,
            timestamp: crate::Timestamp::new(90_000, NonZeroU32::new(90_000).unwrap(), 0).unwrap(),
            payload_type: 107,
            ssrc: 42,
            mark,
            loss: 0,
        }
        .build(payload.iter().copied())
        .unwrap()
    }

    fn message(depacketizer: &mut Depacketizer) -> MessageFrame {
        let Some(Ok(CodecItem::MessageFrame(frame))) = depacketizer.pull() else {
            panic!("expected a complete metadata message");
        };
        assert_eq!(depacketizer.pull(), None);
        frame
    }

    fn received_at(mut packet: ReceivedPacket, received: Instant) -> ReceivedPacket {
        packet.ctx = crate::PacketContext::tcp(
            crate::RtspMessageContext::at(0, received, UNIX_EPOCH).unwrap(),
        );
        packet
    }

    #[test]
    fn single_packet_document_releases_large_transport_allocation() {
        let mut input = packet(0, true, DOCUMENT);
        let wire_length = input.raw().len();
        let mut wire = vec![0; 512 * 1024];
        wire[..wire_length].copy_from_slice(input.raw());
        let backing = bytes::Bytes::from(wire);
        input.raw.0 = backing.slice(..wire_length);
        assert!(!backing.is_unique());
        let mut depacketizer = Depacketizer::new(CompressionType::Uncompressed);
        depacketizer.push(input).unwrap();
        let frame = message(&mut depacketizer);
        assert_eq!(frame.data(), DOCUMENT);
        assert!(
            backing.is_unique(),
            "metadata retained the large transport allocation"
        );
    }

    #[test]
    fn packet_loss_discards_every_partial_document_and_recovers() {
        for split in 1..DOCUMENT.len() {
            for mark in [false, true] {
                let mut depacketizer = Depacketizer::new(CompressionType::Uncompressed);
                depacketizer
                    .push(packet(10, false, &DOCUMENT[..split]))
                    .unwrap();
                assert_eq!(depacketizer.pull(), None);

                let mut suffix = packet(12, mark, &DOCUMENT[split..]);
                suffix.loss = 1;
                depacketizer.push(suffix).unwrap();
                assert_eq!(depacketizer.pull(), None, "split={split}, mark={mark}");
                let next_sequence = if mark {
                    13
                } else {
                    depacketizer.push(packet(13, true, b"")).unwrap();
                    assert_eq!(depacketizer.pull(), None);
                    14
                };

                depacketizer
                    .push(packet(next_sequence, false, &RECOVERED[..8]))
                    .unwrap();
                assert_eq!(depacketizer.pull(), None);
                depacketizer
                    .push(packet(next_sequence + 1, true, &RECOVERED[8..]))
                    .unwrap();
                let frame = message(&mut depacketizer);
                assert_eq!(frame.data(), RECOVERED);
                assert_eq!(frame.loss(), 1);
            }
        }
    }

    #[test]
    fn loss_between_documents_is_saturated_and_reported_once() {
        let mut depacketizer = Depacketizer::new(CompressionType::Uncompressed);
        depacketizer.push(packet(0, true, DOCUMENT)).unwrap();
        assert_eq!(message(&mut depacketizer).data(), DOCUMENT);
        let mut lost = packet(0, false, b"unknown prefix");
        lost.loss = u16::MAX;
        depacketizer.push(lost).unwrap();
        assert_eq!(depacketizer.pull(), None);
        let mut lost_again = packet(3, true, b"tail");
        lost_again.loss = 2;
        depacketizer.push(lost_again).unwrap();
        assert_eq!(depacketizer.pull(), None);
        depacketizer.push(packet(4, true, RECOVERED)).unwrap();
        let frame = message(&mut depacketizer);
        assert_eq!(frame.data(), RECOVERED);
        assert_eq!(frame.loss(), u16::MAX);
        depacketizer.push(packet(5, true, DOCUMENT)).unwrap();
        assert_eq!(message(&mut depacketizer).loss(), 0);
    }

    #[test]
    fn timestamp_change_discards_through_marker_without_fatal_error() {
        for mark in [false, true] {
            let mut depacketizer = Depacketizer::new(CompressionType::Uncompressed);
            depacketizer
                .push(packet(10, false, &DOCUMENT[..8]))
                .unwrap();
            assert_eq!(depacketizer.pull(), None);

            let mut changed = packet(11, mark, &DOCUMENT[8..]);
            changed.timestamp.timestamp += 90_000;
            depacketizer.push(changed).unwrap();
            assert_eq!(depacketizer.pull(), None);
            if !mark {
                depacketizer.push(packet(12, true, b"</Event>")).unwrap();
                assert_eq!(depacketizer.pull(), None);
            }

            depacketizer.push(packet(13, true, RECOVERED)).unwrap();
            let frame = message(&mut depacketizer);
            assert_eq!(frame.data(), RECOVERED);
            assert_eq!(frame.loss(), 0);
        }
    }

    #[test]
    fn oversized_documents_discard_through_marker_and_recover() {
        let payload = vec![0x5a; 32 * 1024];
        for compression in [
            CompressionType::Uncompressed,
            CompressionType::GzipCompressed,
            CompressionType::ExiDefault,
            CompressionType::ExiInBand,
        ] {
            for mark in [false, true] {
                let mut depacketizer = Depacketizer::new(compression);
                for sequence in 0..8 {
                    depacketizer
                        .push(packet(sequence, false, &payload))
                        .unwrap();
                    assert_eq!(depacketizer.pull(), None);
                }
                depacketizer.push(packet(8, mark, b"x")).unwrap();
                assert_eq!(depacketizer.pull(), None);
                assert!(matches!(
                    depacketizer.state,
                    State::Idle | State::Discarding
                ));
                if !mark {
                    depacketizer.push(packet(9, true, b"tail")).unwrap();
                    assert_eq!(depacketizer.pull(), None);
                }
                depacketizer.push(packet(10, true, RECOVERED)).unwrap();
                assert_eq!(message(&mut depacketizer).data(), RECOVERED);
            }
        }
    }

    #[test]
    fn exact_byte_limit_is_accepted_without_excess_assembly_capacity() {
        let document = vec![0x5a; 256 * 1024];
        let chunks: Vec<_> = document.chunks(48 * 1024).collect();
        let mut depacketizer = Depacketizer::new(CompressionType::Uncompressed);
        for (index, chunk) in chunks.iter().enumerate() {
            let mark = index + 1 == chunks.len();
            depacketizer
                .push(packet(u16::try_from(index).unwrap(), mark, chunk))
                .unwrap();
            if let State::InProgress(in_progress) = &depacketizer.state {
                assert!(in_progress.data.len() <= 256 * 1024);
                assert!(in_progress.data.capacity() <= 256 * 1024);
            }
            if !mark {
                assert_eq!(depacketizer.pull(), None);
            }
        }
        assert_eq!(message(&mut depacketizer).data(), document);
    }

    #[test]
    fn large_message_does_not_inflate_next_assembly_high_water() {
        let payload = vec![0x5a; 32 * 1024];
        let mut depacketizer = Depacketizer::new(CompressionType::Uncompressed);
        for sequence in 0..8 {
            depacketizer
                .push(packet(sequence, sequence == 7, &payload))
                .unwrap();
            if sequence < 7 {
                assert_eq!(depacketizer.pull(), None);
            }
        }
        assert_eq!(message(&mut depacketizer).data().len(), 256 * 1024);
        assert!(depacketizer.high_water_size <= 16 * 1024);
        depacketizer
            .push(packet(8, false, &RECOVERED[..8]))
            .unwrap();
        assert_eq!(depacketizer.pull(), None);
        let State::InProgress(in_progress) = &depacketizer.state else {
            panic!("expected a new metadata prefix");
        };
        assert!(in_progress.data.capacity() <= 16 * 1024);
        depacketizer.push(packet(9, true, &RECOVERED[8..])).unwrap();
        assert_eq!(message(&mut depacketizer).data(), RECOVERED);
    }

    #[test]
    fn packet_budget_accepts_marker_on_last_allowed_packet() {
        let mut depacketizer = Depacketizer::new(CompressionType::Uncompressed);
        for sequence in 0..1023 {
            depacketizer.push(packet(sequence, false, b"")).unwrap();
            assert_eq!(depacketizer.pull(), None);
        }
        depacketizer.push(packet(1023, true, DOCUMENT)).unwrap();
        assert_eq!(message(&mut depacketizer).data(), DOCUMENT);
    }

    #[test]
    fn markerless_empty_packets_exhaust_budget_and_recover() {
        let mut depacketizer = Depacketizer::new(CompressionType::Uncompressed);
        for sequence in 0..1024 {
            depacketizer.push(packet(sequence, false, b"")).unwrap();
            assert_eq!(depacketizer.pull(), None);
        }
        assert!(matches!(depacketizer.state, State::Discarding));
        depacketizer.push(packet(1024, true, DOCUMENT)).unwrap();
        assert_eq!(depacketizer.pull(), None);
        depacketizer.push(packet(1025, true, RECOVERED)).unwrap();
        assert_eq!(message(&mut depacketizer).data(), RECOVERED);
    }

    #[test]
    fn receive_deadline_discards_trickled_document_and_recovers() {
        let start = Instant::now();
        for elapsed_ms in [10_000, 10_001] {
            for mark in [false, true] {
                let mut depacketizer = Depacketizer::new(CompressionType::Uncompressed);
                depacketizer
                    .push(received_at(packet(0, false, DOCUMENT), start))
                    .unwrap();
                assert_eq!(depacketizer.pull(), None);
                depacketizer
                    .push(received_at(
                        packet(1, false, b""),
                        start + Duration::from_secs(9),
                    ))
                    .unwrap();
                assert_eq!(depacketizer.pull(), None);
                depacketizer
                    .push(received_at(
                        packet(2, mark, b""),
                        start + Duration::from_millis(elapsed_ms),
                    ))
                    .unwrap();
                assert_eq!(depacketizer.pull(), None);
                assert!(matches!(
                    depacketizer.state,
                    State::Idle | State::Discarding
                ));
                if !mark {
                    depacketizer.push(packet(3, true, b"")).unwrap();
                    assert_eq!(depacketizer.pull(), None);
                }
                depacketizer.push(packet(4, true, RECOVERED)).unwrap();
                assert_eq!(message(&mut depacketizer).data(), RECOVERED);
            }
        }
    }

    #[test]
    fn receive_deadline_accepts_document_just_before_limit() {
        let start = Instant::now();
        let mut depacketizer = Depacketizer::new(CompressionType::Uncompressed);
        let first = received_at(packet(0, false, DOCUMENT), start);
        let expected_ctx = *first.ctx();
        depacketizer.push(first).unwrap();
        assert_eq!(depacketizer.pull(), None);
        depacketizer
            .push(received_at(
                packet(1, true, b""),
                start + Duration::from_millis(9_999),
            ))
            .unwrap();
        let frame = message(&mut depacketizer);
        assert_eq!(frame.data(), DOCUMENT);
        assert_eq!(*frame.ctx(), expected_ctx);
    }

    #[test]
    fn regressing_receive_time_discards_without_panicking() {
        let start = Instant::now();
        let mut depacketizer = Depacketizer::new(CompressionType::Uncompressed);
        depacketizer
            .push(received_at(
                packet(0, false, DOCUMENT),
                start + Duration::from_secs(1),
            ))
            .unwrap();
        assert_eq!(depacketizer.pull(), None);
        depacketizer
            .push(received_at(packet(1, true, b""), start))
            .unwrap();
        assert_eq!(depacketizer.pull(), None);
        depacketizer.push(packet(2, true, RECOVERED)).unwrap();
        assert_eq!(message(&mut depacketizer).data(), RECOVERED);
    }

    #[test]
    fn empty_documents_are_discarded_without_poisoning_next_message() {
        for fragmented in [false, true] {
            let mut depacketizer = Depacketizer::new(CompressionType::Uncompressed);
            if fragmented {
                depacketizer.push(packet(0, false, b"")).unwrap();
                assert_eq!(depacketizer.pull(), None);
            }
            depacketizer.push(packet(1, true, b"")).unwrap();
            assert_eq!(depacketizer.pull(), None);
            depacketizer.push(packet(2, true, RECOVERED)).unwrap();
            assert_eq!(message(&mut depacketizer).data(), RECOVERED);
        }
    }
}
