// Copyright (C) The Retina Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Fixed-size audio sample codecs as defined in
//! [RFC 3551 section 4.5](https://datatracker.ietf.org/doc/html/rfc3551#section-4.5).

use super::{AudioParameters, CodecItem};
use std::num::{NonZeroU16, NonZeroU32};

#[derive(Debug)]
pub struct Depacketizer {
    parameters: AudioParameters,
    pending: Option<super::AudioFrame>,
    bits_per_sample: u32,
}

impl Depacketizer {
    /// Creates a new Depacketizer.
    pub(super) fn new(clock_rate: u32, bits_per_sample: u32, channels: Option<NonZeroU16>) -> Self {
        Self {
            parameters: AudioParameters {
                rfc6381_codec: None,
                frame_length: None, // variable
                clock_rate,
                channels: channels.unwrap_or(const { NonZeroU16::new(1).unwrap() }),
                extra_data: Vec::new(),
                codec: super::AudioParametersCodec::Other,
            },
            bits_per_sample,
            pending: None,
        }
    }

    pub(super) const fn parameters(&self) -> Option<super::ParametersRef<'_>> {
        Some(super::ParametersRef::Audio(&self.parameters))
    }

    /// Returns the frame length in sampling instants per RFC 3551 section 4.
    fn frame_length(&self, payload_len: usize) -> Option<NonZeroU32> {
        assert!(payload_len < usize::from(u16::MAX));
        let bits = (payload_len) as u32 * 8;
        let bits_per_instant = self.bits_per_sample * u32::from(self.parameters.channels.get());
        match bits.is_multiple_of(bits_per_instant) {
            false => None,
            true => NonZeroU32::new(bits / bits_per_instant),
        }
    }

    pub(super) fn push(&mut self, pkt: crate::rtp::ReceivedPacket) -> Result<(), String> {
        assert!(self.pending.is_none());
        let payload = pkt.payload();
        if log::log_enabled!(log::Level::Trace) {
            log::trace!(
                "simple_audio packet: stream_id={} seq={} ts={} loss={} payload_len={} bits_per_sample={} channels={} clock_rate={}",
                pkt.stream_id(),
                pkt.sequence_number(),
                pkt.timestamp().timestamp(),
                pkt.loss(),
                payload.len(),
                self.bits_per_sample,
                self.parameters.channels,
                self.parameters.clock_rate
            );
        }
        let frame_length = self.frame_length(payload.len()).ok_or_else(|| {
            format!(
                "invalid length {} for payload of {}-bit audio samples in {} channel(s)",
                payload.len(),
                self.bits_per_sample,
                self.parameters.channels,
            )
        })?;
        self.pending = Some(super::AudioFrame {
            packet_timestamp: pkt.timestamp(),
            loss: pkt.loss(),
            ctx: *pkt.ctx(),
            stream_id: pkt.stream_id(),
            timestamp: pkt.timestamp(),
            frame_length,
            data: pkt.into_payload_bytes(),
        });
        Ok(())
    }

    pub(super) fn pull(&mut self) -> Option<Result<super::CodecItem, super::DepacketizeError>> {
        self.pending.take().map(|f| Ok(CodecItem::AudioFrame(f)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn depacketizer_8bit() -> Depacketizer {
        Depacketizer::new(8000, 8, None)
    }

    fn depacketizer_16bit() -> Depacketizer {
        Depacketizer::new(16000, 16, None)
    }

    #[test]
    fn frame_length_valid_8bit() {
        // 384 bytes of 8-bit samples = 384 samples (G.711 PCMA/PCMU)
        let d = depacketizer_8bit();
        assert_eq!(d.frame_length(384), Some(NonZeroU32::new(384).unwrap()));
    }

    #[test]
    fn frame_length_invalid_8bit() {
        // Not applicable for 8-bit: any byte count is divisible by 8 bits.
        // But 0 bytes should return None (NonZeroU32::new(0) is None).
        let d = depacketizer_8bit();
        assert_eq!(d.frame_length(0), None);
    }

    #[test]
    fn frame_length_valid_16bit() {
        // 320 bytes = 160 16-bit samples
        let d = depacketizer_16bit();
        assert_eq!(d.frame_length(320), Some(NonZeroU32::new(160).unwrap()));
    }

    #[test]
    fn frame_length_invalid_16bit() {
        // 321 bytes is not divisible by 2 bytes per sample
        let d = depacketizer_16bit();
        assert_eq!(d.frame_length(321), None);
    }

    #[test]
    fn frame_length_stereo_16bit() {
        let d = Depacketizer::new(16000, 16, NonZeroU16::new(2));
        assert_eq!(d.frame_length(320), Some(NonZeroU32::new(80).unwrap()));
        assert_eq!(d.frame_length(322), None);
    }
}
