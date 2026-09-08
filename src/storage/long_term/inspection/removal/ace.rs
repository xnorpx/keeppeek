use std::io;

pub(super) fn allowed(ace: &[u8]) -> io::Result<(u32, &[u8])> {
    if ace.len() < 16 || ace[0] != 0 {
        return Err(super::super::denied());
    }
    let size = usize::from(u16::from_le_bytes([ace[2], ace[3]]));
    if size != ace.len() || ace[8] != 1 || ace[9] > 15 {
        return Err(super::super::denied());
    }
    let sid_bytes = 8 + usize::from(ace[9]) * 4;
    if ace.len() != 8 + sid_bytes {
        return Err(super::super::denied());
    }
    let mask = u32::from_le_bytes([ace[4], ace[5], ace[6], ace[7]]);
    Ok((mask, &ace[8..]))
}

#[cfg(test)]
mod tests {
    use super::allowed;

    #[test]
    fn allowed_aces_require_complete_sid_bytes_before_native_comparison() {
        let valid = [0, 0, 20, 0, 1, 0, 0, 0, 1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0];
        let (mask, sid) = allowed(&valid).unwrap();
        assert_eq!(mask, 1);
        assert_eq!(sid, &valid[8..]);
        for length in 0..valid.len() {
            assert!(allowed(&valid[..length]).is_err());
        }
        for (offset, value) in [(0, 1), (2, 19), (8, 0), (9, 2), (9, 16)] {
            let mut invalid = valid;
            invalid[offset] = value;
            assert!(allowed(&invalid).is_err());
        }
    }
}
