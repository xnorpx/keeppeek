// Copyright (C) The Retina Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Common logic between H.264 and H.265.

use std::borrow::Cow;

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

/// Where an RBSP parsed via [`TolerantBitReader`] ended.
#[derive(Debug, Default)]
pub(super) struct RbspEnd {
    /// True iff there was data other than zero bits after `rbsp_stop_one_bit`,
    /// or the bit where the parse ended was not `1`.
    pub(super) has_extra_trailing_data: bool,

    /// Bit position within the RBSP of `rbsp_stop_one_bit` when found where the parse ended.
    pub(super) stop_bit_pos: Option<u64>,
}

/// `h264_reader::rbsp::BitRead` impl that notes extra trailing data instead of failing.
///
/// Some cameras append stray trailing bytes to parameter sets. Counting the bits read
/// allows [`canonical_nal`] to truncate the NAL after its `rbsp_stop_one_bit`.
#[derive(Debug)]
pub(super) struct TolerantBitReader<'a, R> {
    inner: R,
    pos: u64,
    end: &'a mut RbspEnd,
}

impl<'a, R> TolerantBitReader<'a, R> {
    pub(super) const fn new(inner: R, end: &'a mut RbspEnd) -> Self {
        Self { inner, pos: 0, end }
    }

    #[inline]
    fn advance<T>(&mut self, r: T, bits: impl FnOnce(&T) -> u64) -> T {
        self.pos += bits(&r);
        r
    }
}

/// Returns the bit length of the unsigned Exp-Golomb code for `v`.
#[inline]
const fn ue_bits(v: u32) -> u64 {
    2 * ((v as u64 + 1).ilog2() as u64) + 1
}

impl<R: h264_reader::rbsp::BitRead> h264_reader::rbsp::BitRead for TolerantBitReader<'_, R> {
    fn read_ue(&mut self, name: &'static str) -> Result<u32, h264_reader::rbsp::BitReaderError> {
        let r = self.inner.read_ue(name);
        self.advance(r, |r| r.as_ref().map_or(0, |&v| ue_bits(v)))
    }

    fn read_se(&mut self, name: &'static str) -> Result<i32, h264_reader::rbsp::BitReaderError> {
        let r = self.inner.read_se(name);
        self.advance(r, |r| {
            r.as_ref().map_or(0, |&v| {
                // Inverse of the mapping in H.264 section 9.1.1, Table 9-3.
                let v = i64::from(v);
                let k = if v > 0 { 2 * v - 1 } else { -2 * v };
                ue_bits(u32::try_from(k).expect("se(v) maps from a u32 ue(v)"))
            })
        })
    }

    fn read_bit(&mut self, name: &'static str) -> Result<bool, h264_reader::rbsp::BitReaderError> {
        let r = self.inner.read_bit(name);
        self.advance(r, |r| u64::from(r.is_ok()))
    }

    fn skip(
        &mut self,
        bit_count: u32,
        name: &'static str,
    ) -> Result<(), h264_reader::rbsp::BitReaderError> {
        let r = self.inner.skip(bit_count, name);
        self.advance(r, |r| if r.is_ok() { u64::from(bit_count) } else { 0 })
    }

    fn read<const BITS: u32, I: h264_reader::rbsp::Integer>(
        &mut self,
        name: &'static str,
    ) -> Result<I, h264_reader::rbsp::BitReaderError> {
        let r = self.inner.read::<BITS, I>(name);
        self.advance(r, |r| if r.is_ok() { u64::from(BITS) } else { 0 })
    }

    fn read_var<I: h264_reader::rbsp::Integer>(
        &mut self,
        bit_count: u32,
        name: &'static str,
    ) -> Result<I, h264_reader::rbsp::BitReaderError> {
        let r = self.inner.read_var(bit_count, name);
        self.advance(r, |r| if r.is_ok() { u64::from(bit_count) } else { 0 })
    }

    fn read_to<V: h264_reader::rbsp::Primitive>(
        &mut self,
        name: &'static str,
    ) -> Result<V, h264_reader::rbsp::BitReaderError> {
        let r = self.inner.read_to(name);
        self.advance(r, |r| {
            if r.is_ok() {
                (std::mem::size_of::<V>() * 8) as u64
            } else {
                0
            }
        })
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

    fn finish_rbsp(mut self) -> Result<(), h264_reader::rbsp::BitReaderError> {
        use h264_reader::rbsp::BitReaderError;

        // Read `rbsp_stop_one_bit` directly because `inner.finish_rbsp` returns
        // `RemainingData` for both missing stop bits and stray trailing data.
        if !self.inner.read_bit("rbsp_stop_one_bit")? {
            self.end.has_extra_trailing_data = true;
            return Ok(());
        }

        // Only zero bits should follow `rbsp_stop_one_bit`; `inner.finish_rbsp`
        // reports a clean end as `UnexpectedEof` after the stop bit is consumed.
        match self.inner.finish_rbsp() {
            Err(BitReaderError::ReaderError(_, e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof => {}
            Ok(()) | Err(BitReaderError::RemainingData) => {
                self.end.has_extra_trailing_data = true;
            }
            Err(e) => return Err(e),
        }
        self.end.stop_bit_pos = Some(self.pos);
        Ok(())
    }

    fn finish_sei_payload(self) -> Result<(), h264_reader::rbsp::BitReaderError> {
        self.inner.finish_sei_payload()
    }
}

/// Returns the canonical form of a parameter set NAL parsed via [`TolerantBitReader`].
///
/// The canonical NAL ends at the byte containing `rbsp_stop_one_bit`, with any lower bits
/// in that byte zeroed and any trailing bytes dropped. Only call this for parameter sets
/// whose syntax does not use `more_rbsp_data()` lookahead.
pub(super) fn canonical_nal<'n>(nal: &'n [u8], hdr_len: usize, end: &RbspEnd) -> Cow<'n, [u8]> {
    let Some(pos) = end.stop_bit_pos else {
        debug_assert!(end.has_extra_trailing_data, "no stop bit position");
        return Cow::Borrowed(nal);
    };
    let Ok(rbsp_byte) = usize::try_from(pos / 8) else {
        return Cow::Borrowed(nal);
    };
    let stop_bit = 0x80u8 >> (pos % 8);

    // Map `rbsp_byte` into `nal` while skipping emulation prevention `0x03` bytes.
    let mut zeros = 0;
    let mut i_rbsp = 0;
    let mut off = None;
    for (i, &b) in nal.iter().enumerate().skip(hdr_len) {
        if zeros >= 2 && b == 0x03 {
            zeros = 0;
            continue;
        }
        if i_rbsp == rbsp_byte {
            off = Some(i);
            break;
        }
        i_rbsp += 1;
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    let Some(off) = off.filter(|&off| nal[off] & stop_bit != 0) else {
        debug_assert!(false, "stop bit {pos} not found in {nal:02x?}");
        return Cow::Borrowed(nal);
    };

    let last = nal[off] & !(stop_bit - 1);
    debug_assert!(
        end.has_extra_trailing_data
            || (last == nal[off] && nal[off + 1..].iter().all(|&b| b == 0 || b == 3)),
        "stop bit {pos} is followed by data in {nal:02x?}, yet no trailing data was reported",
    );
    if off + 1 == nal.len() && last == nal[off] {
        return Cow::Borrowed(nal);
    }
    let mut canonical = nal[..=off].to_vec();
    canonical[off] = last;
    Cow::Owned(canonical)
}

/// Returns true iff the parameter set NAL in `chunks` is equivalent to `stored`.
///
/// When `tolerant` is true, NALs match if they are identical through `stored`'s last `1`
/// bit, ignoring trailing bytes or bits after the stop bit. When false, NALs must match
/// byte-for-byte.
pub(super) fn set_matches<'c>(
    stored: &[u8],
    tolerant: bool,
    chunks: impl IntoIterator<Item = &'c [u8]>,
) -> bool {
    let (mut prefix, last) = tolerant
        .then(|| stored.iter().rposition(|&b| b != 0))
        .flatten()
        .map_or((stored, None), |i| (&stored[..i], Some(stored[i])));
    for mut chunk in chunks {
        let n = prefix.len().min(chunk.len());
        if chunk[..n] != prefix[..n] {
            return false;
        }
        prefix = &prefix[n..];
        chunk = &chunk[n..];
        if !prefix.is_empty() {
            continue;
        }
        match (last, chunk.first()) {
            (_, None) => {}
            (None, Some(_)) => return false,
            (Some(last), Some(&b)) => {
                let mask = !((last & last.wrapping_neg()) - 1);
                return b & mask == last;
            }
        }
    }
    prefix.is_empty() && last.is_none()
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

    #[test]
    fn ue_bits() {
        for (v, bits) in [
            (0, 1),
            (1, 3),
            (2, 3),
            (3, 5),
            (6, 5),
            (7, 7),
            (u32::MAX - 1, 63),
        ] {
            assert_eq!(super::ue_bits(v), bits, "v={v}");
        }
    }

    #[test]
    fn tolerant_bit_reader_canonical() {
        use h264_reader::rbsp::BitRead as _;

        let nal = b"\x67\x90\xaa\xb5\xb7\x55";
        let mut end = RbspEnd::default();
        let mut r = TolerantBitReader::new(
            h264_reader::rbsp::BitReader::new(h264_reader::rbsp::ByteReader::without_skip(
                &nal[1..],
            )),
            &mut end,
        );
        assert_eq!(r.read_ue("a").unwrap(), 0);
        assert_eq!(r.read_ue("b").unwrap(), 3);
        assert_eq!(r.read_se("c").unwrap(), -2);
        assert_eq!(r.read_se("d").unwrap(), 1);
        assert_eq!(r.read::<4, u8>("e").unwrap(), 0b1010);
        r.skip(1, "f").unwrap();
        assert_eq!(r.read_to::<u8>("g").unwrap(), 0xad);
        r.finish_rbsp().unwrap();
        assert!(end.has_extra_trailing_data);
        assert_eq!(end.stop_bit_pos, Some(27));
        assert_eq!(&*canonical_nal(nal, 1, &end), b"\x67\x90\xaa\xb5\xb0");

        let end = RbspEnd {
            has_extra_trailing_data: false,
            stop_bit_pos: Some(27),
        };
        let canonical = canonical_nal(b"\x67\x90\xaa\xb5\xb0", 1, &end);
        assert!(matches!(canonical, Cow::Borrowed(_)));
        assert_eq!(
            &*canonical_nal(b"\x67\x90\xaa\xb5\xb0\x00\x00", 1, &end),
            b"\x67\x90\xaa\xb5\xb0"
        );
    }

    #[test]
    fn tolerant_bit_reader_finish() {
        use h264_reader::rbsp::BitRead as _;
        let finish = |nal: &[u8]| {
            let mut end = RbspEnd::default();
            let mut r = TolerantBitReader::new(
                h264_reader::rbsp::BitReader::new(h264_reader::rbsp::ByteReader::without_skip(
                    &nal[1..],
                )),
                &mut end,
            );
            assert_eq!(r.read_ue("a").unwrap(), 0);
            r.finish_rbsp().unwrap();
            (end.has_extra_trailing_data, end.stop_bit_pos)
        };

        assert_eq!(finish(b"\x67\xc0"), (false, Some(1)));
        assert_eq!(finish(b"\x67\xc0\x00"), (false, Some(1)));
        assert_eq!(finish(b"\x67\xc1"), (true, Some(1)));
        assert_eq!(finish(b"\x67\xc0\x01"), (true, Some(1)));

        let nal = b"\x67\x90";
        assert_eq!(finish(nal), (true, None));
        let end = RbspEnd {
            has_extra_trailing_data: true,
            stop_bit_pos: None,
        };
        assert!(matches!(canonical_nal(nal, 1, &end), Cow::Borrowed(n) if n == nal));
    }

    #[test]
    fn canonical_nal_emulation_prevention() {
        let end = RbspEnd {
            has_extra_trailing_data: true,
            stop_bit_pos: Some(24),
        };
        assert_eq!(
            &*canonical_nal(b"\x42\x01\x00\x00\x03\x01\x80\xff", 2, &end),
            b"\x42\x01\x00\x00\x03\x01\x80"
        );
    }

    #[test]
    fn set_matches() {
        let stored = b"\x67\x90\xaa\xa0";
        let tolerant = |chunks: &[&[u8]]| super::set_matches(stored, true, chunks.iter().copied());
        let exact = |chunks: &[&[u8]]| super::set_matches(stored, false, chunks.iter().copied());
        for (chunks, want_tolerant, want_exact) in [
            (&[&b"\x67\x90\xaa\xa0"[..]][..], true, true),
            (&[b"\x67", b"\x90\xaa", b"", b"\xa0"], true, true),
            (&[b"\x67\x90\xaa\xa7\x55"], true, false),
            (&[b"\x67\x90", b"\xaa", b"\xbf"], true, false),
            (&[b"\x67\x90\xaa\xa0\x00"], true, false),
            (&[b"\x67\x90\xaa"], false, false),
            (&[b"\x67\x90\xaa", b""], false, false),
            (&[b"\x67\x90\xaa\x80"], false, false),
            (&[b"\x67\x90\xaa\xc0"], false, false),
            (&[b"\x67\x90\xab\xa0"], false, false),
            (&[b"\x68\x90\xaa\xa0"], false, false),
        ] {
            assert_eq!(tolerant(chunks), want_tolerant, "tolerant {chunks:02x?}");
            assert_eq!(exact(chunks), want_exact, "exact {chunks:02x?}");
        }

        assert!(super::set_matches(
            b"\x67\x90\xaa\xa0\x00",
            true,
            [&b"\x67\x90\xaa\xa1"[..]]
        ));
    }
}
