use crate::{error::BcError, header::PacketHeader, magic::*};

/// A complete, raw Baichuan message (header + body bytes, before decryption).
#[derive(Debug)]
pub struct RawMessage {
    pub header: PacketHeader,
    /// Body bytes (body_len bytes, still potentially encrypted).
    pub body: Vec<u8>,
}

/// Accumulates TCP byte stream and extracts complete Baichuan messages.
pub struct ReadBuffer {
    buf: Vec<u8>,
}

impl ReadBuffer {
    pub const fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Number of buffered bytes.
    pub const fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether the buffer is empty.
    pub const fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Append incoming TCP bytes to the buffer.
    pub fn extend(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Try to extract the next complete message.
    ///
    /// Returns `Ok(Some(msg))` if a complete message is available.
    /// Returns `Ok(None)` if more data is needed.
    /// Returns `Err` if the data is malformed.
    pub fn try_parse_message(&mut self) -> Result<Option<RawMessage>, BcError> {
        if self.buf.len() < HEADER_LEN_SHORT {
            return Ok(None);
        }

        // Scan for magic if the first bytes don't match.
        if !has_header_magic(&self.buf) {
            if let Some(offset) = self.scan_for_magic() {
                // Discard bytes before the magic.
                self.buf.drain(..offset);
                if self.buf.len() < HEADER_LEN_SHORT {
                    return Ok(None);
                }
            } else {
                // No magic found in the entire buffer. Keep the last 3 bytes
                // (in case magic straddles the boundary) and discard the rest.
                let keep = self.buf.len().min(3);
                let drain_to = self.buf.len() - keep;
                self.buf.drain(..drain_to);
                return Ok(None);
            }
        }

        // Try to parse the header.
        let (header, header_len) = match PacketHeader::parse(&self.buf) {
            Ok(v) => v,
            Err(BcError::Incomplete) => return Ok(None),
            Err(e) => return Err(e),
        };

        let body_len = header.body_len as usize;
        if body_len > crate::MAX_SNAPSHOT_BYTES {
            return Err(BcError::MessageTooLarge {
                size: body_len,
                max: crate::MAX_SNAPSHOT_BYTES,
            });
        }
        let total_len = header_len
            .checked_add(body_len)
            .ok_or(BcError::InvalidHeader("message length overflow"))?;
        if self.buf.len() < total_len {
            return Ok(None);
        }

        // Extract the body.
        let body = self.buf[header_len..total_len].to_vec();

        // Compact: remove the consumed bytes.
        self.buf.drain(..total_len);

        Ok(Some(RawMessage { header, body }))
    }

    /// Scan the buffer for the BC magic bytes. Returns the offset if found.
    fn scan_for_magic(&self) -> Option<usize> {
        // Start from index 1 since index 0 was already checked.
        (1..self.buf.len().saturating_sub(3)).find(|&i| has_header_magic(&self.buf[i..]))
    }
}

impl Default for ReadBuffer {
    fn default() -> Self {
        Self::new()
    }
}
