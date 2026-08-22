use chacha20poly1305::{
    ChaCha20Poly1305, Key, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use hkdf::Hkdf;
use sha2::Sha512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    KeyDerivation,
    Encrypt,
    Authenticate,
}

pub fn derive<const N: usize>(
    input_key_material: &[u8],
    salt: &[u8],
    info: &[u8],
) -> Result<[u8; N], Error> {
    let mut output = [0; N];
    Hkdf::<Sha512>::new(Some(salt), input_key_material)
        .expand(info, &mut output)
        .map_err(|_| Error::KeyDerivation)?;
    Ok(output)
}

pub fn label_nonce(label: &[u8; 8]) -> [u8; 12] {
    let mut nonce = [0; 12];
    nonce[4..].copy_from_slice(label);
    nonce
}

pub fn counter_nonce(counter: u64) -> [u8; 12] {
    let mut nonce = [0; 12];
    nonce[4..].copy_from_slice(&counter.to_le_bytes());
    nonce
}

pub fn seal(
    key: &[u8; 32],
    nonce: &[u8; 12],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, Error> {
    ChaCha20Poly1305::new(Key::from_slice(key))
        .encrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| Error::Encrypt)
}

pub fn open(
    key: &[u8; 32],
    nonce: &[u8; 12],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>, Error> {
    ChaCha20Poly1305::new(Key::from_slice(key))
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| Error::Authenticate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_and_counter_nonces_have_hap_layout() {
        assert_eq!(label_nonce(b"PV-Msg02"), *b"\0\0\0\0PV-Msg02");
        assert_eq!(
            counter_nonce(0x0807_0605_0403_0201),
            [0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8]
        );
    }

    #[test]
    fn seal_and_open_authenticate_aad() {
        let key = [7; 32];
        let nonce = label_nonce(b"PV-Msg02");
        let sealed = seal(&key, &nonce, b"aad", b"plaintext").unwrap();

        assert_eq!(open(&key, &nonce, b"aad", &sealed).unwrap(), b"plaintext");
        assert_eq!(
            open(&key, &nonce, b"other", &sealed),
            Err(Error::Authenticate)
        );
    }
}
