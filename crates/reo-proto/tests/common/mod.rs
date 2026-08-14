use reo_proto::magic::BC_MAGIC;

/// Build raw header bytes for testing. Produces 20 or 24 bytes depending
/// on whether `extension` is provided.
pub fn make_header_bytes(
    msg_id: u32,
    body_len: u32,
    encryption_offset: u32,
    status_class: u32,
    extension: Option<u32>,
) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&BC_MAGIC.to_le_bytes());
    buf.extend_from_slice(&msg_id.to_le_bytes());
    buf.extend_from_slice(&body_len.to_le_bytes());
    buf.extend_from_slice(&encryption_offset.to_le_bytes());
    buf.extend_from_slice(&status_class.to_le_bytes());
    if let Some(ext) = extension {
        buf.extend_from_slice(&ext.to_le_bytes());
    }
    buf
}
