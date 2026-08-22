use crate::crypto;
use std::{error::Error as StdError, fmt};
use zeroize::{Zeroize, ZeroizeOnDrop};

const MAX_BLOCK_SIZE: usize = 1024;
const LENGTH_SIZE: usize = 2;
const TAG_SIZE: usize = 16;
const CONTROL_SALT: &[u8] = b"Control-Salt";
const CONTROL_READ_INFO: &[u8] = b"Control-Read-Encryption-Key";
const CONTROL_WRITE_INFO: &[u8] = b"Control-Write-Encryption-Key";

/// Directional keys established by a successful HAP Pair Verify exchange.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct SessionKeys {
    accessory_to_controller: [u8; 32],
    controller_to_accessory: [u8; 32],
}

impl SessionKeys {
    /// Derives directional control keys from the X25519 Pair Verify secret.
    pub fn derive(shared_secret: &[u8; 32]) -> Result<Self, RecordError> {
        Ok(Self {
            accessory_to_controller: crypto::derive(shared_secret, CONTROL_SALT, CONTROL_READ_INFO)
                .map_err(|_| RecordError::KeyDerivation)?,
            controller_to_accessory: crypto::derive(
                shared_secret,
                CONTROL_SALT,
                CONTROL_WRITE_INFO,
            )
            .map_err(|_| RecordError::KeyDerivation)?,
        })
    }

    /// Creates an encoder for accessory-to-controller HAP traffic.
    pub const fn encoder(&self) -> RecordEncoder {
        RecordEncoder::new(self.accessory_to_controller)
    }

    /// Creates a decoder for controller-to-accessory HAP traffic.
    pub const fn decoder(&self) -> RecordDecoder {
        RecordDecoder::new(self.controller_to_accessory)
    }
}

impl fmt::Debug for SessionKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionKeys").finish_non_exhaustive()
    }
}

/// Encrypts plaintext HAP bytes into authenticated control records.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct RecordEncoder {
    key: [u8; 32],
    counter: Counter,
}

impl RecordEncoder {
    const fn new(key: [u8; 32]) -> Self {
        Self {
            key,
            counter: Counter::new(),
        }
    }

    /// Encrypts an arbitrary plaintext stream into records of at most 1024 bytes.
    pub fn encode(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, RecordError> {
        let mut output = Vec::with_capacity(
            plaintext.len() + plaintext.len().div_ceil(MAX_BLOCK_SIZE) * (LENGTH_SIZE + TAG_SIZE),
        );
        for block in plaintext.chunks(MAX_BLOCK_SIZE) {
            self.encode_block(block, &mut output)?;
        }
        Ok(output)
    }

    fn encode_block(&mut self, block: &[u8], output: &mut Vec<u8>) -> Result<(), RecordError> {
        let length = u16::try_from(block.len()).map_err(|_| RecordError::InvalidLength {
            actual: block.len(),
            maximum: MAX_BLOCK_SIZE,
        })?;
        if block.len() > MAX_BLOCK_SIZE {
            return Err(RecordError::InvalidLength {
                actual: block.len(),
                maximum: MAX_BLOCK_SIZE,
            });
        }
        let aad = length.to_le_bytes();
        let nonce = self.counter.nonce()?;
        let sealed =
            crypto::seal(&self.key, &nonce, &aad, block).map_err(|_| RecordError::Encrypt)?;
        self.counter.advance();
        output.extend_from_slice(&aad);
        output.extend_from_slice(&sealed);
        Ok(())
    }
}

/// Decrypts complete HAP control records from a caller-managed byte buffer.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct RecordDecoder {
    key: [u8; 32],
    counter: Counter,
}

impl RecordDecoder {
    const fn new(key: [u8; 32]) -> Self {
        Self {
            key,
            counter: Counter::new(),
        }
    }

    /// Attempts to decode exactly one record from the front of `input`.
    pub fn decode(&mut self, input: &[u8]) -> Result<DecodeResult, RecordError> {
        if input.len() < LENGTH_SIZE {
            return Ok(DecodeResult::NeedMore {
                total_length: LENGTH_SIZE,
            });
        }
        let declared = usize::from(u16::from_le_bytes([input[0], input[1]]));
        if declared > MAX_BLOCK_SIZE {
            return Err(RecordError::InvalidLength {
                actual: declared,
                maximum: MAX_BLOCK_SIZE,
            });
        }
        let total_length = LENGTH_SIZE + declared + TAG_SIZE;
        if input.len() < total_length {
            return Ok(DecodeResult::NeedMore { total_length });
        }

        let nonce = self.counter.nonce()?;
        let plaintext = crypto::open(
            &self.key,
            &nonce,
            &input[..LENGTH_SIZE],
            &input[LENGTH_SIZE..total_length],
        )
        .map_err(|_| RecordError::Authenticate)?;
        self.counter.advance();
        Ok(DecodeResult::Decoded {
            plaintext,
            consumed: total_length,
        })
    }
}

/// Result of attempting to decode one encrypted HAP record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeResult {
    /// More socket bytes are needed before the record can be authenticated.
    NeedMore {
        /// Total bytes required for the current record.
        total_length: usize,
    },
    /// One authenticated plaintext block was decoded.
    Decoded {
        /// Decrypted HAP stream bytes.
        plaintext: Vec<u8>,
        /// Number of encrypted input bytes consumed.
        consumed: usize,
    },
}

#[derive(Clone, Copy, Zeroize)]
struct Counter {
    value: u64,
    exhausted: bool,
}

impl Counter {
    const fn new() -> Self {
        Self {
            value: 0,
            exhausted: false,
        }
    }

    fn nonce(self) -> Result<[u8; 12], RecordError> {
        if self.exhausted {
            return Err(RecordError::CounterExhausted);
        }
        Ok(crypto::counter_nonce(self.value))
    }

    const fn advance(&mut self) {
        if self.value == u64::MAX {
            self.exhausted = true;
        } else {
            self.value += 1;
        }
    }
}

/// HAP encrypted record failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordError {
    /// A record claimed or supplied more plaintext than HAP permits.
    InvalidLength { actual: usize, maximum: usize },
    /// HKDF could not produce the requested fixed-size key.
    KeyDerivation,
    /// In-memory AEAD encryption failed.
    Encrypt,
    /// Authentication or decryption failed.
    Authenticate,
    /// Continuing would reuse a ChaCha20-Poly1305 nonce.
    CounterExhausted,
}

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLength { actual, maximum } => {
                write!(
                    f,
                    "record has {actual} plaintext bytes; maximum is {maximum}"
                )
            }
            Self::KeyDerivation => f.write_str("unable to derive HAP session key"),
            Self::Encrypt => f.write_str("unable to encrypt HAP record"),
            Self::Authenticate => f.write_str("HAP record authentication failed"),
            Self::CounterExhausted => f.write_str("HAP record nonce counter exhausted"),
        }
    }
}

impl StdError for RecordError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_and_round_trips_plaintext_stream() {
        let key = [0x42; 32];
        let mut encoder = RecordEncoder::new(key);
        let mut decoder = RecordDecoder::new(key);
        let plaintext = vec![0x5a; 2050];
        let encrypted = encoder.encode(&plaintext).unwrap();
        let mut offset = 0;
        let mut decoded = Vec::new();

        while offset < encrypted.len() {
            let DecodeResult::Decoded {
                plaintext,
                consumed,
            } = decoder.decode(&encrypted[offset..]).unwrap()
            else {
                panic!("complete encrypted stream must decode");
            };
            decoded.extend_from_slice(&plaintext);
            offset += consumed;
        }

        assert_eq!(decoded, plaintext);
        assert_eq!(
            encrypted.len(),
            plaintext.len() + 3 * (LENGTH_SIZE + TAG_SIZE)
        );
    }

    #[test]
    fn reports_required_partial_frame_length() {
        let key = [0x42; 32];
        let encrypted = RecordEncoder::new(key).encode(b"hello").unwrap();
        let mut decoder = RecordDecoder::new(key);

        assert_eq!(
            decoder.decode(&encrypted[..1]).unwrap(),
            DecodeResult::NeedMore {
                total_length: LENGTH_SIZE,
            }
        );
        assert_eq!(
            decoder.decode(&encrypted[..5]).unwrap(),
            DecodeResult::NeedMore {
                total_length: LENGTH_SIZE + 5 + TAG_SIZE,
            }
        );
    }

    #[test]
    fn rejects_tampering_without_advancing_counter() {
        let key = [0x42; 32];
        let encrypted = RecordEncoder::new(key).encode(b"hello").unwrap();
        let mut tampered = encrypted.clone();
        *tampered.last_mut().unwrap() ^= 1;
        let mut decoder = RecordDecoder::new(key);

        assert_eq!(decoder.decode(&tampered), Err(RecordError::Authenticate));
        assert!(matches!(
            decoder.decode(&encrypted).unwrap(),
            DecodeResult::Decoded { .. }
        ));
    }

    #[test]
    fn refuses_nonce_reuse_after_counter_exhaustion() {
        let mut counter = Counter {
            value: u64::MAX,
            exhausted: false,
        };
        assert!(counter.nonce().is_ok());
        counter.advance();
        assert_eq!(counter.nonce(), Err(RecordError::CounterExhausted));
    }

    #[test]
    fn directional_keys_are_not_interchangeable() {
        let keys = SessionKeys::derive(&[0x42; 32]).unwrap();
        let encrypted = keys.encoder().encode(b"accessory response").unwrap();

        assert_eq!(
            keys.decoder().decode(&encrypted),
            Err(RecordError::Authenticate)
        );
    }
}
