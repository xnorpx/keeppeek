// Copyright (C) The Retina Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Common logic between H.264 and H.265.

/// Annex B start code with the `zero_byte` prefix.
///
/// The `zero_byte` is mandatory before parameter sets (SPS, PPS, VPS) and before the first NAL
/// unit of each access unit (H.264 Annex B section B.1.2; H.265 has equivalent language).
/// We use the 4-byte form everywhere for simplicity.
pub const ANNEX_B_START_CODE: [u8; 4] = [0, 0, 0, 1];

/// How to frame H.26x NAL units in output (packet format and extra data format).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Framing {
    /// AVCC/HCC style: NALs use 4-byte big-endian length prefix, extra data in `AvcDecoderConfigurationRecord` or `HevcDecoderConfigurationRecord`. Default.
    #[default]
    FourByteLength,

    /// Annex B start codes (`00 00 00 01`) for both packet data and extra data.
    AnnexB,
}

/// `h264_reader::rbsp::BitRead` impl that *notes* extra trailing data rather than failing on it.
///
/// Some (Reolink) cameras appear to have a stray extra byte at the end. Follow the lead of most
/// other RTSP implementations in tolerating this.
#[derive(Debug)]
pub(super) struct TolerantBitReader<'a, R> {
    pub(super) inner: R,
    pub(super) has_extra_trailing_data: &'a mut bool,
}

impl<R: h264_reader::rbsp::BitRead> h264_reader::rbsp::BitRead for TolerantBitReader<'_, R> {
    fn read_ue(&mut self, name: &'static str) -> Result<u32, h264_reader::rbsp::BitReaderError> {
        self.inner.read_ue(name)
    }

    fn read_se(&mut self, name: &'static str) -> Result<i32, h264_reader::rbsp::BitReaderError> {
        self.inner.read_se(name)
    }

    fn read_bit(&mut self, name: &'static str) -> Result<bool, h264_reader::rbsp::BitReaderError> {
        self.inner.read_bit(name)
    }

    fn skip(
        &mut self,
        bit_count: u32,
        name: &'static str,
    ) -> Result<(), h264_reader::rbsp::BitReaderError> {
        self.inner.skip(bit_count, name)
    }

    fn read<const BITS: u32, I: h264_reader::rbsp::Integer>(
        &mut self,
        name: &'static str,
    ) -> Result<I, h264_reader::rbsp::BitReaderError> {
        self.inner.read::<BITS, I>(name)
    }

    fn read_var<I: h264_reader::rbsp::Integer>(
        &mut self,
        bit_count: u32,
        name: &'static str,
    ) -> Result<I, h264_reader::rbsp::BitReaderError> {
        self.inner.read_var(bit_count, name)
    }

    fn read_to<V: h264_reader::rbsp::Primitive>(
        &mut self,
        name: &'static str,
    ) -> Result<V, h264_reader::rbsp::BitReaderError> {
        self.inner.read_to(name)
    }

    fn byte_aligned(&self) -> bool {
        self.inner.byte_aligned()
    }

    fn has_more_rbsp_data(
        &mut self,
        name: &'static str,
    ) -> Result<bool, h264_reader::rbsp::BitReaderError> {
        self.inner.has_more_rbsp_data(name)
    }

    fn finish_rbsp(self) -> Result<(), h264_reader::rbsp::BitReaderError> {
        match self.inner.finish_rbsp() {
            Ok(()) => Ok(()),
            Err(h264_reader::rbsp::BitReaderError::RemainingData) => {
                *self.has_extra_trailing_data = true;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    fn finish_sei_payload(self) -> Result<(), h264_reader::rbsp::BitReaderError> {
        self.inner.finish_sei_payload()
    }
}

/// [`std::io::BufRead`] adapter over a slice of [`bytes::Bytes`] NAL pieces without copying.
#[derive(Clone)]
pub(super) struct PiecesReader<'a> {
    pieces: &'a [bytes::Bytes],
    cur: &'a [u8],
}

impl<'a> PiecesReader<'a> {
    pub(super) const fn new(pieces: &'a [bytes::Bytes]) -> Self {
        Self { pieces, cur: &[] }
    }
}

impl std::io::Read for PiecesReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        use std::io::BufRead as _;
        let chunk = self.fill_buf()?;
        let amt = std::cmp::min(out.len(), chunk.len());
        out[..amt].copy_from_slice(&chunk[..amt]);
        self.consume(amt);
        Ok(amt)
    }
}

impl std::io::BufRead for PiecesReader<'_> {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        while self.cur.is_empty() {
            let Some((piece, rest)) = self.pieces.split_first() else {
                break;
            };
            self.cur = piece;
            self.pieces = rest;
        }
        Ok(self.cur)
    }

    fn consume(&mut self, amt: usize) {
        self.cur = &self.cur[amt..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    #[test]
    fn pieces_reader_handles_split_emulation_prevention() {
        let pieces = [
            bytes::Bytes::from_static(b"\x01\x00\x00"),
            bytes::Bytes::from_static(b"\x03\x01\x02\x03\x04"),
        ];
        let mut rbsp = Vec::new();
        h264_reader::rbsp::ByteReader::without_skip(PiecesReader::new(&pieces))
            .read_to_end(&mut rbsp)
            .unwrap();
        assert_eq!(rbsp, b"\x01\x00\x00\x01\x02\x03\x04");
    }
}
