use aes::Aes128;
use cfb_mode::{Decryptor as CfbDecryptor, Encryptor as CfbEncryptor, cipher::KeyIvInit};
use md5::{Digest, Md5};

/// BCEncrypt XOR key (8 bytes, repeating).
const BC_XOR_KEY: [u8; 8] = [0x1F, 0x2D, 0x3C, 0x4B, 0x5A, 0x69, 0x78, 0xFF];

/// Fixed AES-128-CFB initialization vector.
const AES_IV: [u8; 16] = *b"0123456789abcdef";

/// Apply BCEncrypt XOR cipher in-place.
///
/// XORs each byte of `data` with the repeating 8-byte key, cycling from
/// position `channel_id % 8`, and additionally XORing each byte with
/// `channel_id`.  For channel 0 this is equivalent to a plain key XOR.
pub fn bc_xor(data: &mut [u8], channel_id: u8) {
    let skip = (channel_id as usize) % BC_XOR_KEY.len();
    for (i, byte) in data.iter_mut().enumerate() {
        *byte ^= BC_XOR_KEY[(skip + i) % BC_XOR_KEY.len()] ^ channel_id;
    }
}

/// Derive a 16-byte AES key from a nonce and password.
///
/// Computes `MD5("{nonce}-{password}")`, converts to uppercase hex,
/// and takes the first 16 characters as the key.
pub fn derive_aes_key(nonce: &str, password: &str) -> [u8; 16] {
    let mut hasher = Md5::new();
    hasher.update(nonce.as_bytes());
    hasher.update(b"-");
    hasher.update(password.as_bytes());
    let digest = hasher.finalize();

    // Convert to uppercase hex string (32 chars) then take first 16
    let mut hex_buf = [0u8; 32];
    hex_encode_upper(&digest, &mut hex_buf);

    let mut key = [0u8; 16];
    key.copy_from_slice(&hex_buf[..16]);
    key
}

/// Compute the credential hash used in the modern login request.
///
/// `MD5("{value}{nonce}")`, uppercase hex, truncated to 31 characters.
pub fn credential_hash(nonce: &str, value: &str) -> [u8; 31] {
    let mut hasher = Md5::new();
    hasher.update(value.as_bytes());
    hasher.update(nonce.as_bytes());
    let digest = hasher.finalize();

    let mut hex_buf = [0u8; 32];
    hex_encode_upper(&digest, &mut hex_buf);

    let mut result = [0u8; 31];
    result.copy_from_slice(&hex_buf[..31]);
    result
}

/// AES-128-CFB cipher state.
///
/// Persists across TCP chunks for Full AES mode. The cipher state must
/// only be reset when a new Baichuan message header is detected.
pub struct AesCipherState {
    key: [u8; 16],
}

impl AesCipherState {
    /// Create a new AES cipher state from a derived key.
    pub const fn new(key: [u8; 16]) -> Self {
        Self { key }
    }

    /// Create from nonce and password (derives the key internally).
    pub fn from_credentials(nonce: &str, password: &str) -> Self {
        Self::new(derive_aes_key(nonce, password))
    }

    /// The raw 16-byte key.
    pub const fn key(&self) -> &[u8; 16] {
        &self.key
    }

    /// Encrypt data in-place using AES-128-CFB with the fixed IV.
    ///
    /// Each call creates a fresh cipher (IV reset). For stateful
    /// encryption across TCP chunks, call `encrypt_continuing` instead.
    pub fn encrypt(&self, data: &mut [u8]) {
        let encryptor = CfbEncryptor::<Aes128>::new((&self.key).into(), (&AES_IV).into());
        encryptor.encrypt(data);
    }

    /// Decrypt data in-place using AES-128-CFB with the fixed IV.
    ///
    /// Each call creates a fresh cipher (IV reset). For stateful
    /// decryption across TCP chunks, call `decrypt_continuing` instead.
    pub fn decrypt(&self, data: &mut [u8]) {
        let decryptor = CfbDecryptor::<Aes128>::new((&self.key).into(), (&AES_IV).into());
        decryptor.decrypt(data);
    }
}

/// Encrypt a message body in-place using BCEncrypt XOR cipher.
pub fn encrypt_body_xor(data: &mut [u8], channel_id: u8) {
    bc_xor(data, channel_id);
}

/// Decrypt a message body in-place using BCEncrypt XOR cipher.
///
/// XOR is symmetric, so this is identical to `encrypt_body_xor`.
pub fn decrypt_body_xor(data: &mut [u8], channel_id: u8) {
    bc_xor(data, channel_id);
}

/// Encrypt a message body in-place with AES, respecting the encryption offset.
pub fn encrypt_body_aes(cipher: &AesCipherState, data: &mut [u8], encryption_offset: usize) {
    if encryption_offset < data.len() {
        cipher.encrypt(&mut data[encryption_offset..]);
    }
}

/// Decrypt a message body in-place with AES, respecting the encryption offset.
pub fn decrypt_body_aes(cipher: &AesCipherState, data: &mut [u8], encryption_offset: usize) {
    if encryption_offset < data.len() {
        cipher.decrypt(&mut data[encryption_offset..]);
    }
}

/// Encode a byte slice as uppercase hexadecimal into a fixed buffer.
fn hex_encode_upper(bytes: &[u8], out: &mut [u8]) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for (i, &b) in bytes.iter().enumerate() {
        out[i * 2] = HEX[(b >> 4) as usize];
        out[i * 2 + 1] = HEX[(b & 0x0f) as usize];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xor_roundtrip() {
        let original = b"Hello, Baichuan!".to_vec();
        let mut data = original.clone();
        bc_xor(&mut data, 0);
        // encrypted should differ from original
        assert_ne!(data, original);
        // decrypt (XOR is symmetric)
        bc_xor(&mut data, 0);
        assert_eq!(data, original);
    }

    #[test]
    fn xor_with_channel() {
        let original = vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        let mut data = original.clone();
        // Encrypt with channel_id = 1
        encrypt_body_xor(&mut data, 1);
        // All bytes changed (channel affects output)
        assert_ne!(data, original);
        // Decrypt (XOR is symmetric)
        decrypt_body_xor(&mut data, 1);
        assert_eq!(data, original);
    }

    #[test]
    fn xor_channel_affects_output() {
        let mut data_ch0 = [0u8; 8];
        let mut data_ch1 = [0u8; 8];
        bc_xor(&mut data_ch0, 0);
        bc_xor(&mut data_ch1, 1);
        // Different channels produce different output
        assert_ne!(data_ch0, data_ch1);
    }

    #[test]
    fn aes_key_derivation() {
        let key = derive_aes_key("ABCDEF123456", "admin123");
        // Key should be 16 ASCII bytes (uppercase hex chars)
        assert_eq!(key.len(), 16);
        for &b in &key {
            assert!(
                b.is_ascii_hexdigit() && (b.is_ascii_uppercase() || b.is_ascii_digit()),
                "key byte {b:02x} is not uppercase hex"
            );
        }
    }

    #[test]
    fn aes_encrypt_decrypt_roundtrip() {
        let cipher = AesCipherState::from_credentials("testnonce", "testpass");
        let original = b"This is a test XML body for Baichuan protocol.".to_vec();
        let mut data = original.clone();

        cipher.encrypt(&mut data);
        assert_ne!(data, original);

        cipher.decrypt(&mut data);
        assert_eq!(data, original);
    }

    #[test]
    fn aes_body_with_offset() {
        let cipher = AesCipherState::from_credentials("nonce123", "password");
        let mut data = vec![0u8; 32];
        for (i, b) in data.iter_mut().enumerate() {
            *b = i as u8;
        }
        let original = data.clone();

        encrypt_body_aes(&cipher, &mut data, 8);
        // First 8 bytes unchanged
        assert_eq!(&data[..8], &original[..8]);
        // Rest encrypted
        assert_ne!(&data[8..], &original[8..]);

        decrypt_body_aes(&cipher, &mut data, 8);
        assert_eq!(data, original);
    }

    #[test]
    fn credential_hash_length_and_format() {
        let hash = credential_hash("ABCDEF", "admin");
        assert_eq!(hash.len(), 31);
        for &b in &hash {
            assert!(
                b.is_ascii_hexdigit() && (b.is_ascii_uppercase() || b.is_ascii_digit()),
                "hash byte {b:02x} is not uppercase hex"
            );
        }
    }

    #[test]
    fn credential_hash_deterministic() {
        let h1 = credential_hash("nonce", "pass");
        let h2 = credential_hash("nonce", "pass");
        assert_eq!(h1, h2);
    }

    #[test]
    fn credential_hash_differs_with_different_inputs() {
        let h1 = credential_hash("nonce1", "pass");
        let h2 = credential_hash("nonce2", "pass");
        assert_ne!(h1, h2);
    }

    #[test]
    fn xor_known_answer_zeros() {
        // XOR of 8 zero bytes should produce the key itself
        let mut data = [0u8; 8];
        bc_xor(&mut data, 0);
        assert_eq!(data, BC_XOR_KEY);
    }

    #[test]
    fn xor_known_answer_hello() {
        // XOR of b"Hello, B" with key at offset 0
        let mut data = *b"Hello, B";
        bc_xor(&mut data, 0);
        assert_eq!(data, [0x57, 0x48, 0x50, 0x27, 0x35, 0x45, 0x58, 0xBD]);
    }

    #[test]
    fn aes_key_derivation_known_answer() {
        // MD5("ABCDEF123456-admin123") = "5a9dcd3cb12caff4..." (uppercase)
        // First 16 chars = "5A9DCD3CB12CAFF4"
        let key = derive_aes_key("ABCDEF123456", "admin123");
        assert_eq!(&key, b"5A9DCD3CB12CAFF4");
    }

    #[test]
    fn credential_hash_known_answer() {
        // MD5("adminABCDEF") uppercase hex, first 31 chars
        let hash = credential_hash("ABCDEF", "admin");
        assert_eq!(&hash, b"69B3F7A9B9DA9CC372FA63F07BF26D1");
    }

    #[test]
    fn aes_encrypt_empty_data() {
        let cipher = AesCipherState::from_credentials("n", "p");
        let mut data = vec![];
        cipher.encrypt(&mut data);
        assert!(data.is_empty());
        cipher.decrypt(&mut data);
        assert!(data.is_empty());
    }

    #[test]
    fn aes_body_offset_beyond_len() {
        let cipher = AesCipherState::from_credentials("n", "p");
        let mut data = vec![1, 2, 3, 4];
        let original = data.clone();
        // offset beyond data length: nothing happens
        encrypt_body_aes(&cipher, &mut data, 100);
        assert_eq!(data, original);
        decrypt_body_aes(&cipher, &mut data, 100);
        assert_eq!(data, original);
    }

    #[test]
    fn aes_key_accessor() {
        let key = [0x41u8; 16];
        let cipher = AesCipherState::new(key);
        assert_eq!(cipher.key(), &key);
    }
}
