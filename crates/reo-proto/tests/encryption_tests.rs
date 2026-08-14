use reo_proto::encryption::*;

#[test]
fn test_xor_empty_data() {
    let mut data = vec![];
    bc_xor(&mut data, 0);
    assert!(data.is_empty());
}

#[test]
fn test_xor_single_byte() {
    let mut data = vec![0x00];
    bc_xor(&mut data, 0);
    // 0x00 XOR 0x1F = 0x1F
    assert_eq!(data[0], 0x1F);
    bc_xor(&mut data, 0);
    assert_eq!(data[0], 0x00);
}

#[test]
fn test_xor_key_wraps() {
    // Data longer than 8 bytes should wrap the key
    let mut data = vec![0u8; 16];
    let original = data.clone();
    bc_xor(&mut data, 0);
    // Bytes 0 and 8 should be the same (same key byte)
    assert_eq!(data[0], data[8]);
    assert_eq!(data[1], data[9]);
    // Roundtrip
    bc_xor(&mut data, 0);
    assert_eq!(data, original);
}

#[test]
fn test_xor_with_channel_id() {
    let mut data1 = vec![0x42u8; 4];
    let mut data2 = vec![0x42u8; 4];
    bc_xor(&mut data1, 0);
    bc_xor(&mut data2, 3);
    // Should produce different results due to different channel_id
    assert_ne!(data1, data2);
}

#[test]
fn test_aes_key_is_uppercase_hex() {
    let key = derive_aes_key("test_nonce", "secretpassword");
    for &b in &key {
        assert!(
            b.is_ascii_digit() || (b'A'..=b'F').contains(&b),
            "key byte {b:#04x} should be uppercase hex ASCII"
        );
    }
}

#[test]
fn test_aes_roundtrip_various_sizes() {
    let cipher = AesCipherState::from_credentials("nonce", "password");
    for size in [1, 7, 16, 17, 100, 1000] {
        let original: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let mut data = original.clone();
        cipher.encrypt(&mut data);
        assert_ne!(data, original, "size {size} should be encrypted");
        cipher.decrypt(&mut data);
        assert_eq!(data, original, "size {size} roundtrip failed");
    }
}

#[test]
fn test_credential_hash_truncated_to_31() {
    let hash = credential_hash("somenonce", "somevalue");
    assert_eq!(hash.len(), 31);
}

#[test]
fn test_body_encrypt_decrypt_xor_roundtrip() {
    let original: Vec<u8> = (0..64).collect();
    let mut data = original.clone();
    encrypt_body_xor(&mut data, 0);
    assert_ne!(data, original);
    decrypt_body_xor(&mut data, 0);
    assert_eq!(data, original);
}

#[test]
fn test_body_encrypt_decrypt_aes_roundtrip() {
    let cipher = AesCipherState::from_credentials("n", "p");
    let original: Vec<u8> = (0..64).collect();
    let mut data = original.clone();
    encrypt_body_aes(&cipher, &mut data, 0);
    assert_ne!(data, original);
    decrypt_body_aes(&cipher, &mut data, 0);
    assert_eq!(data, original);
}
