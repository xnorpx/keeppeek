use sha2::{Digest, Sha256};
use std::{
    io::{self, Read, Seek, SeekFrom},
    time::{Duration, Instant},
};

pub(super) const CHUNK_BYTES: usize = 65_536;
use crate::shares::MAX_MEDIA_BYTES as MAX_BYTES;
pub(super) const VERIFY_TIMEOUT: Duration = Duration::from_secs(25);

/// Pins each returned chunk to the whole artifact digest established during preflight.
/// Same-length mutation after preflight cannot change the bytes returned to a guest.
pub(super) struct Verified<R> {
    reader: R,
    bytes: u64,
    digests: Vec<[u8; 32]>,
    buffer: Box<[u8]>,
    loaded: Option<usize>,
    position: u64,
}

impl<R: Read + Seek> Verified<R> {
    pub fn new(
        mut reader: R,
        bytes: u64,
        expected: [u8; 32],
        mut check: impl FnMut() -> io::Result<()>,
    ) -> io::Result<Self> {
        if bytes == 0 || bytes > MAX_BYTES {
            return Err(unavailable());
        }
        let count =
            usize::try_from(bytes.div_ceil(CHUNK_BYTES as u64)).map_err(|_| unavailable())?;
        let deadline = Instant::now()
            .checked_add(VERIFY_TIMEOUT)
            .ok_or_else(unavailable)?;
        let mut digests = Vec::with_capacity(count);
        let mut buffer = vec![0; CHUNK_BYTES].into_boxed_slice();
        let mut digest = Sha256::new();
        reader.seek(SeekFrom::Start(0))?;
        let mut remaining = bytes;
        for _ in 0..count {
            check()?;
            if Instant::now() >= deadline {
                return Err(unavailable());
            }
            let length =
                usize::try_from(remaining.min(CHUNK_BYTES as u64)).map_err(|_| unavailable())?;
            reader.read_exact(&mut buffer[..length])?;
            digest.update(&buffer[..length]);
            digests.push(Sha256::digest(&buffer[..length]).into());
            remaining -= u64::try_from(length).map_err(|_| unavailable())?;
        }
        check()?;
        if remaining != 0
            || Instant::now() >= deadline
            || <[u8; 32]>::from(digest.finalize()) != expected
        {
            return Err(unavailable());
        }
        Ok(Self {
            reader,
            bytes,
            digests,
            buffer,
            loaded: None,
            position: 0,
        })
    }

    pub fn peek(&mut self, maximum: usize) -> io::Result<&[u8]> {
        if maximum == 0 || self.position == self.bytes {
            return Ok(&[]);
        }
        let index =
            usize::try_from(self.position / CHUNK_BYTES as u64).map_err(|_| unavailable())?;
        self.load(index)?;
        let offset =
            usize::try_from(self.position % CHUNK_BYTES as u64).map_err(|_| unavailable())?;
        let remaining = usize::try_from(self.bytes - self.position).map_err(|_| unavailable())?;
        let length = maximum.min(CHUNK_BYTES - offset).min(remaining);
        Ok(&self.buffer[offset..offset + length])
    }

    pub fn consume(&mut self, length: usize) {
        let offset =
            usize::try_from(self.position % CHUNK_BYTES as u64).expect("chunk offset fits usize");
        assert!(
            length <= CHUNK_BYTES - offset,
            "guest read cannot cross a verified chunk"
        );
        let next = self
            .position
            .checked_add(u64::try_from(length).expect("bounded read fits u64"))
            .expect("bounded guest position cannot overflow");
        assert!(
            next <= self.bytes,
            "guest read cannot cross the artifact boundary"
        );
        self.position = next;
    }
    fn load(&mut self, index: usize) -> io::Result<()> {
        if self.loaded == Some(index) {
            return Ok(());
        }
        let start = u64::try_from(index)
            .map_err(|_| unavailable())?
            .checked_mul(CHUNK_BYTES as u64)
            .ok_or_else(unavailable)?;
        let remaining = self.bytes.checked_sub(start).ok_or_else(unavailable)?;
        let length =
            usize::try_from(remaining.min(CHUNK_BYTES as u64)).map_err(|_| unavailable())?;
        let expected = self.digests.get(index).ok_or_else(unavailable)?;
        self.loaded = None;
        self.reader.seek(SeekFrom::Start(start))?;
        self.reader.read_exact(&mut self.buffer[..length])?;
        if <[u8; 32]>::from(Sha256::digest(&self.buffer[..length])) != *expected {
            self.loaded = None;
            return Err(unavailable());
        }
        self.loaded = Some(index);
        Ok(())
    }
}

impl<R: Read + Seek> Read for Verified<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let bytes = self.peek(output.len())?;
        let length = bytes.len();
        output[..length].copy_from_slice(bytes);
        self.consume(length);
        Ok(length)
    }
}

impl<R: Read + Seek> Seek for Verified<R> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let position = match from {
            SeekFrom::Start(position) => i128::from(position),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
            SeekFrom::End(offset) => i128::from(self.bytes) + i128::from(offset),
        };
        if position < 0 || position > i128::from(self.bytes) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "guest seek exceeds artifact",
            ));
        }
        self.position = u64::try_from(position).map_err(|_| unavailable())?;
        Ok(self.position)
    }
}

pub(super) fn unavailable() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "guest artifact is unavailable")
}

#[cfg(test)]
#[path = "verified_tests.rs"]
mod tests;
