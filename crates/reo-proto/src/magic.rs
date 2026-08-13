/// Baichuan protocol magic: 0x0ABCDEF0 in little-endian.
pub const BC_MAGIC: u32 = 0x0ABC_DEF0;

/// Baichuan magic as raw bytes (little-endian).
pub const BC_MAGIC_BYTES: [u8; 4] = [0xf0, 0xde, 0xbc, 0x0a];

/// Reversed-endian variant seen in JPEG snapshot payloads.
pub const JPEG_MAGIC: u32 = 0x0FED_CBA0;

/// Standard header length (20 bytes).
pub const HEADER_LEN_SHORT: usize = 20;

/// Extended header length (24 bytes, with extension field).
pub const HEADER_LEN_EXTENDED: usize = 24;

//
// The Baichuan header stores `response_code` (u16 LE at bytes 16..18)
// and `class` (u16 LE at bytes 18..20) packed into a single u32 field:
//
//   status_class = (class << 16) | response_code
//
// The class determines the header length and message type.

/// Legacy / binary class — 20-byte header (no extension field).
pub const BC_CLASS_LEGACY: u16 = 0x6514;

/// Modern XML without extension — 20-byte header.
/// Used for nonce responses, pings, and simple modern messages.
pub const BC_CLASS_MODERN_SHORT: u16 = 0x6614;

/// Modern XML with extension — 24-byte header (has payload_offset).
pub const BC_CLASS_MODERN_EXT: u16 = 0x6414;

/// Build the `status_class` u32 from a protocol class and response code.
#[inline]
pub const fn make_status(class: u16, response_code: u16) -> u32 {
    ((class as u32) << 16) | (response_code as u32)
}
