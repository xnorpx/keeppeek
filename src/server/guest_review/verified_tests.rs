use super::*;
use std::io::Cursor;

fn prepared(bytes: Vec<u8>) -> Verified<Cursor<Vec<u8>>> {
    let digest = Sha256::digest(&bytes).into();
    Verified::new(
        Cursor::new(bytes.clone()),
        bytes.len() as u64,
        digest,
        || Ok(()),
    )
    .unwrap()
}

#[test]
fn preflight_bounds_digest_state_and_cache_without_whole_artifact_copy() {
    let bytes = vec![17; CHUNK_BYTES + 7];
    let mut verified = prepared(bytes.clone());
    assert_eq!(verified.digests.len(), 2);
    assert_eq!(verified.buffer.len(), CHUNK_BYTES);
    let mut output = vec![0; bytes.len()];
    verified.read_exact(&mut output).unwrap();
    assert_eq!(output, bytes);
    assert_eq!(verified.read(&mut [0]).unwrap(), 0);
}

#[test]
fn changed_unread_chunk_returns_no_bytes_even_when_file_length_is_unchanged() {
    let mut verified = prepared(vec![17; CHUNK_BYTES + 7]);
    verified.reader.get_mut()[CHUNK_BYTES] = 99;
    verified.seek(SeekFrom::Start(CHUNK_BYTES as u64)).unwrap();
    let mut output = [0; 7];
    assert!(verified.read(&mut output).is_err());
    assert_eq!(output, [0; 7]);
}

#[test]
fn already_verified_cached_bytes_stay_original_after_same_chunk_mutation() {
    let mut verified = prepared(vec![17; CHUNK_BYTES]);
    assert_eq!(verified.read(&mut [0]).unwrap(), 1);
    verified.reader.get_mut()[1] = 99;
    let mut output = [0; 7];
    verified.read_exact(&mut output).unwrap();
    assert_eq!(output, [17; 7]);
}

#[test]
fn checksum_truncation_cancellation_and_size_fail_before_reader_is_returned() {
    assert!(Verified::new(Cursor::new(vec![17; 3]), 3, [0; 32], || Ok(())).is_err());
    assert!(Verified::new(Cursor::new(vec![17; 3]), 4, [0; 32], || Ok(())).is_err());
    assert!(Verified::new(Cursor::new(vec![17; 3]), 3, [0; 32], || Err(unavailable())).is_err());
    assert!(Verified::new(Cursor::new(Vec::new()), 0, [0; 32], || Ok(())).is_err());
    assert!(Verified::new(Cursor::new(Vec::new()), MAX_BYTES + 1, [0; 32], || Ok(())).is_err());
}

#[test]
fn checked_seeks_preserve_range_and_do_not_allocate() {
    let mut verified = prepared(vec![17; 3]);
    assert!(verified.seek(SeekFrom::Start(4)).is_err());
    assert!(verified.seek(SeekFrom::Current(-1)).is_err());
    assert!(verified.seek(SeekFrom::End(i64::MAX)).is_err());
    assert_eq!(verified.seek(SeekFrom::End(-1)).unwrap(), 2);
    assert_eq!(verified.seek(SeekFrom::Current(1)).unwrap(), 3);
    assert_eq!(verified.read(&mut [0]).unwrap(), 0);
    assert_eq!(verified.seek(SeekFrom::Start(0)).unwrap(), 0);
}

#[test]
fn failed_partial_chunk_load_cannot_relabel_old_cache_after_seek() {
    let mut bytes = vec![17; CHUNK_BYTES];
    bytes.extend([22; 7]);
    let mut verified = prepared(bytes);
    verified.read_exact(&mut [0; 7]).unwrap();
    verified.reader.get_mut().truncate(CHUNK_BYTES + 2);
    verified.seek(SeekFrom::Start(CHUNK_BYTES as u64)).unwrap();
    assert!(verified.read_exact(&mut [0; 7]).is_err());
    verified.seek(SeekFrom::Start(0)).unwrap();
    let mut output = [0; 7];
    verified.read_exact(&mut output).unwrap();
    assert_eq!(output, [17; 7]);
}
