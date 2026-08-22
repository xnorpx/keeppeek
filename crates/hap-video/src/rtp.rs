//! RFC 6184 H.264 RTP packetization for the legacy HomeKit camera path.
//!
//! Access units arrive length-prefixed (AVCC). Each NAL is emitted either as a
//! single NAL unit packet or, when it exceeds the negotiated MTU, as a sequence
//! of FU-A fragments. STAP-A aggregation is not used; HomeKit does not require
//! it and single NAL packets keep the fragmentation logic auditable.

use std::{error::Error as StdError, fmt};

const RTP_FIXED_HEADER_LEN: usize = 12;
const FU_A_TYPE: u8 = 28;
const FU_HEADER_LEN: usize = 2;
const NAL_LENGTH_PREFIX: usize = 4;

/// Smallest payload that can still carry one FU-A fragment byte.
const MIN_PAYLOAD_LEN: usize = FU_HEADER_LEN + 1;

/// Failure while packetizing an access unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketizeError {
    /// The configured payload budget cannot carry an FU-A fragment.
    PayloadTooSmall,
    /// A length prefix ran past the end of the access unit.
    TruncatedAccessUnit,
    /// A NAL unit declared zero length.
    EmptyNal,
}

impl fmt::Display for PacketizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PayloadTooSmall => f.write_str("payload budget cannot carry an FU-A fragment"),
            Self::TruncatedAccessUnit => f.write_str("NAL length prefix exceeds the access unit"),
            Self::EmptyNal => f.write_str("access unit contains a zero-length NAL"),
        }
    }
}

impl StdError for PacketizeError {}

/// Builds RTP packets for one H.264 synchronization source.
#[derive(Debug)]
pub struct H264Packetizer {
    payload_type: u8,
    ssrc: u32,
    sequence: u16,
    max_payload: usize,
}

impl H264Packetizer {
    /// Creates a packetizer, where `max_payload` excludes the RTP header and
    /// the SRTP authentication tag.
    pub const fn new(
        payload_type: u8,
        ssrc: u32,
        initial_sequence: u16,
        max_payload: usize,
    ) -> Self {
        Self {
            payload_type,
            ssrc,
            sequence: initial_sequence,
            max_payload,
        }
    }

    /// Returns the sequence number the next emitted packet will carry.
    pub const fn next_sequence(&self) -> u16 {
        self.sequence
    }

    /// Appends the RTP packets for one AVCC access unit to `output`.
    ///
    /// The marker bit is set on the final packet, signalling the end of the
    /// access unit to the depacketizer.
    pub fn packetize(
        &mut self,
        access_unit: &[u8],
        timestamp: u32,
        output: &mut Vec<Vec<u8>>,
    ) -> Result<(), PacketizeError> {
        if self.max_payload < MIN_PAYLOAD_LEN {
            return Err(PacketizeError::PayloadTooSmall);
        }
        let first = output.len();
        let mut offset = 0;
        while offset < access_unit.len() {
            if offset + NAL_LENGTH_PREFIX > access_unit.len() {
                return Err(PacketizeError::TruncatedAccessUnit);
            }
            let length = u32::from_be_bytes([
                access_unit[offset],
                access_unit[offset + 1],
                access_unit[offset + 2],
                access_unit[offset + 3],
            ]) as usize;
            offset += NAL_LENGTH_PREFIX;
            if length == 0 {
                return Err(PacketizeError::EmptyNal);
            }
            if offset + length > access_unit.len() {
                return Err(PacketizeError::TruncatedAccessUnit);
            }
            self.emit_nal(&access_unit[offset..offset + length], timestamp, output);
            offset += length;
        }
        if output.len() > first
            && let Some(last) = output.last_mut()
        {
            last[1] |= 0x80;
        }
        Ok(())
    }

    fn emit_nal(&mut self, nal: &[u8], timestamp: u32, output: &mut Vec<Vec<u8>>) {
        if nal.len() <= self.max_payload {
            output.push(self.packet(timestamp, nal));
            return;
        }
        let header = nal[0];
        let indicator = (header & 0xE0) | FU_A_TYPE;
        let nal_type = header & 0x1F;
        let budget = self.max_payload - FU_HEADER_LEN;
        let mut body = &nal[1..];
        let mut start = true;
        while !body.is_empty() {
            let take = budget.min(body.len());
            let last = take == body.len();
            let mut fu_header = nal_type;
            if start {
                fu_header |= 0x80;
            }
            if last {
                fu_header |= 0x40;
            }
            let mut payload = Vec::with_capacity(FU_HEADER_LEN + take);
            payload.push(indicator);
            payload.push(fu_header);
            payload.extend_from_slice(&body[..take]);
            output.push(self.packet(timestamp, &payload));
            body = &body[take..];
            start = false;
        }
    }

    fn packet(&mut self, timestamp: u32, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(RTP_FIXED_HEADER_LEN + payload.len());
        out.push(0x80);
        out.push(self.payload_type & 0x7F);
        out.extend_from_slice(&self.sequence.to_be_bytes());
        out.extend_from_slice(&timestamp.to_be_bytes());
        out.extend_from_slice(&self.ssrc.to_be_bytes());
        out.extend_from_slice(payload);
        self.sequence = self.sequence.wrapping_add(1);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn avcc(nals: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for nal in nals {
            out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            out.extend_from_slice(nal);
        }
        out
    }

    fn payload(packet: &[u8]) -> &[u8] {
        &packet[RTP_FIXED_HEADER_LEN..]
    }

    #[test]
    fn emits_one_packet_per_small_nal_and_marks_the_last() {
        let mut packetizer = H264Packetizer::new(99, 0x1234_5678, 7, 1_200);
        let mut out = Vec::new();
        packetizer
            .packetize(
                &avcc(&[&[0x67, 1, 2], &[0x68, 3], &[0x65, 4, 5]]),
                900,
                &mut out,
            )
            .unwrap();

        assert_eq!(out.len(), 3);
        assert_eq!(payload(&out[0]), &[0x67, 1, 2]);
        assert_eq!(payload(&out[2]), &[0x65, 4, 5]);
        assert_eq!(out[0][1] & 0x80, 0, "only the final packet is marked");
        assert_eq!(out[1][1] & 0x80, 0);
        assert_eq!(out[2][1] & 0x80, 0x80);
        assert_eq!(u16::from_be_bytes([out[0][2], out[0][3]]), 7);
        assert_eq!(u16::from_be_bytes([out[2][2], out[2][3]]), 9);
        assert_eq!(packetizer.next_sequence(), 10);
    }

    #[test]
    fn fragments_a_large_nal_into_fu_a() {
        let mut packetizer = H264Packetizer::new(99, 1, 0, 10);
        let mut nal = vec![0x65];
        nal.extend((0..40_u8).map(|byte| byte + 1));
        let mut out = Vec::new();
        packetizer.packetize(&avcc(&[&nal]), 0, &mut out).unwrap();

        assert!(out.len() > 1);
        let mut reassembled = vec![0x65];
        for (index, packet) in out.iter().enumerate() {
            let body = payload(packet);
            assert_eq!(body[0], 0x60 | FU_A_TYPE, "FU indicator keeps nal_ref_idc");
            assert_eq!(body[1] & 0x1F, 0x05, "FU header carries the original type");
            assert_eq!(
                body[1] & 0x80 != 0,
                index == 0,
                "start bit only on the first"
            );
            assert_eq!(
                body[1] & 0x40 != 0,
                index == out.len() - 1,
                "end bit only on the last"
            );
            reassembled.extend_from_slice(&body[2..]);
        }
        assert_eq!(reassembled, nal);
        assert_eq!(out.last().unwrap()[1] & 0x80, 0x80);
    }

    #[test]
    fn rejects_a_truncated_length_prefix() {
        let mut packetizer = H264Packetizer::new(99, 1, 0, 1_200);
        let mut out = Vec::new();
        let mut data = avcc(&[&[0x65, 1, 2, 3]]);
        data.truncate(6);
        assert_eq!(
            packetizer.packetize(&data, 0, &mut out),
            Err(PacketizeError::TruncatedAccessUnit)
        );
    }

    #[test]
    fn rejects_zero_length_and_undersized_budgets() {
        let mut packetizer = H264Packetizer::new(99, 1, 0, 1_200);
        let mut out = Vec::new();
        assert_eq!(
            packetizer.packetize(&[0, 0, 0, 0], 0, &mut out),
            Err(PacketizeError::EmptyNal)
        );
        let mut tiny = H264Packetizer::new(99, 1, 0, 2);
        assert_eq!(
            tiny.packetize(&avcc(&[&[0x65, 1]]), 0, &mut out),
            Err(PacketizeError::PayloadTooSmall)
        );
    }
}
