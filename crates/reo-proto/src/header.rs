use crate::{error::BcError, magic::*};

/// Parsed Baichuan message header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketHeader {
    /// Command identifier (see message IDs).
    pub msg_id: u32,
    /// Size of payload in bytes.
    pub body_len: u32,
    /// Bytes 12..16: packs channel_id (u8), stream_type (u8), msg_num (u16).
    /// For most messages this is 0.
    pub encryption_offset: u32,
    /// Bytes 16..20: packs response_code (u16 LE) and class (u16 LE).
    /// Use `make_status(class, response_code)` to build this value.
    pub status_class: u32,
    /// Extension field (present in 24-byte headers).
    pub extension: Option<u32>,
}

impl PacketHeader {
    /// Camera channel encoded in bytes 12 through 13.
    pub const fn channel_id(&self) -> u8 {
        self.encryption_offset as u8
    }

    /// Stream selector encoded in bytes 13 through 14.
    pub const fn stream_type_id(&self) -> u8 {
        (self.encryption_offset >> 8) as u8
    }

    /// Request handle encoded in bytes 14 through 16.
    pub const fn message_number(&self) -> u16 {
        (self.encryption_offset >> 16) as u16
    }

    /// Return this header with a request handle encoded in bytes 14 through 16.
    pub const fn with_message_number(mut self, message_number: u16) -> Self {
        self.encryption_offset =
            (self.encryption_offset & 0x0000_FFFF) | ((message_number as u32) << 16);
        self
    }

    /// Total header length in bytes (20 or 24).
    pub const fn header_len(&self) -> usize {
        if self.extension.is_some() {
            HEADER_LEN_EXTENDED
        } else {
            HEADER_LEN_SHORT
        }
    }

    /// The protocol class from the upper 16 bits of status_class.
    pub const fn bc_class(&self) -> u16 {
        ((self.status_class >> 16) & 0xFFFF) as u16
    }

    /// The response code from the lower 16 bits of status_class.
    pub const fn response_code(&self) -> u16 {
        (self.status_class & 0xFFFF) as u16
    }

    /// Whether the body is binary (legacy class).
    pub const fn is_binary(&self) -> bool {
        self.bc_class() == BC_CLASS_LEGACY
    }

    /// Whether this uses the modern (XML-based) protocol.
    pub const fn is_modern(&self) -> bool {
        matches!(
            self.bc_class(),
            BC_CLASS_MODERN_EXT | BC_CLASS_MODERN_SHORT | 0x0000
        )
    }

    /// Whether the 24-byte extended header is present.
    pub const fn is_extended(&self) -> bool {
        matches!(self.bc_class(), BC_CLASS_MODERN_EXT | 0x0000)
    }

    /// Parse a header from a byte slice.
    ///
    /// Returns the parsed header and the number of bytes consumed.
    /// Returns `Err(BcError::Incomplete)` if not enough bytes.
    /// Returns `Err(BcError::BadMagic)` if magic doesn't match.
    pub const fn parse(data: &[u8]) -> Result<(Self, usize), BcError> {
        if data.len() < HEADER_LEN_SHORT {
            return Err(BcError::Incomplete);
        }

        let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        if !is_header_magic(magic) {
            return Err(BcError::BadMagic([data[0], data[1], data[2], data[3]]));
        }

        let msg_id = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let body_len = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        let encryption_offset = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
        let status_class = u32::from_le_bytes([data[16], data[17], data[18], data[19]]);

        let class = ((status_class >> 16) & 0xFFFF) as u16;
        let has_extension = class == BC_CLASS_MODERN_EXT || class == 0x0000;

        if has_extension {
            if data.len() < HEADER_LEN_EXTENDED {
                return Err(BcError::Incomplete);
            }
            let extension = u32::from_le_bytes([data[20], data[21], data[22], data[23]]);
            Ok((
                Self {
                    msg_id,
                    body_len,
                    encryption_offset,
                    status_class,
                    extension: Some(extension),
                },
                HEADER_LEN_EXTENDED,
            ))
        } else {
            Ok((
                Self {
                    msg_id,
                    body_len,
                    encryption_offset,
                    status_class,
                    extension: None,
                },
                HEADER_LEN_SHORT,
            ))
        }
    }

    /// Serialize this header into a fixed-size buffer.
    ///
    /// Writes into the provided 24-byte buffer and returns the number of
    /// bytes written (20 or 24).
    pub fn serialize(&self, buf: &mut [u8; HEADER_LEN_EXTENDED]) -> usize {
        buf[0..4].copy_from_slice(&BC_MAGIC.to_le_bytes());
        buf[4..8].copy_from_slice(&self.msg_id.to_le_bytes());
        buf[8..12].copy_from_slice(&self.body_len.to_le_bytes());
        buf[12..16].copy_from_slice(&self.encryption_offset.to_le_bytes());
        buf[16..20].copy_from_slice(&self.status_class.to_le_bytes());
        self.extension.map_or(HEADER_LEN_SHORT, |ext| {
            buf[20..24].copy_from_slice(&ext.to_le_bytes());
            HEADER_LEN_EXTENDED
        })
    }
}
