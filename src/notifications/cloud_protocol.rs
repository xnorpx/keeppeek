//! Compact 109-byte signed wire protocol for Keeppeek Service NVR event signals.
//!
//! Each outbound signal contains only the protocol version, opaque 16-byte NVR ID,
//! random 16-byte `cloud_event_id`, whole-second Unix timestamp, signing key version,
//! and Ed25519 signature. No camera, event, media, or credential metadata can enter the body.

use std::fmt;

use aws_lc_rs::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};

/// Protocol version byte for canonical NVR event signals.
pub const PROTOCOL_VERSION_V1: u8 = 1;

/// Exact byte length of a canonical version 1 signal body.
pub const CANONICAL_BODY_BYTES: usize = 109;

/// Hard pre-parse ceiling on signal bodies accepted by client or server.
pub const PRE_PARSE_CEILING_BYTES: usize = 256;

/// Prefix length of the unsigned body bytes (`0..45`).
const UNSIGNED_BODY_BYTES: usize = 45;

/// Fixed Ed25519 signature length in bytes.
const SIGNATURE_BYTES: usize = 64;

/// Fixed domain separator prepended to bytes `0..45` before Ed25519 signing.
pub const DOMAIN_SEPARATOR: &[u8] = b"keeppeek.nvr-event.v1\0";

/// Total length of the signed transcript (`DOMAIN_SEPARATOR` + unsigned body).
const TRANSCRIPT_BYTES: usize = DOMAIN_SEPARATOR.len() + UNSIGNED_BODY_BYTES;

const _: () = {
    assert!(CANONICAL_BODY_BYTES == UNSIGNED_BODY_BYTES + SIGNATURE_BYTES);
    assert!(CANONICAL_BODY_BYTES <= PRE_PARSE_CEILING_BYTES);
    assert!(DOMAIN_SEPARATOR.len() == 22);
    assert!(DOMAIN_SEPARATOR[DOMAIN_SEPARATOR.len() - 1] == 0);
};

/// Verified fields extracted from a canonical 109-byte Keeppeek Service event signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerifiedSignal {
    pub version: u8,
    pub nvr_id: [u8; 16],
    pub cloud_event_id: [u8; 16],
    pub issued_at_secs: u64,
    pub signing_key_version: u32,
    pub signature: [u8; SIGNATURE_BYTES],
}

/// Unsigned parameters required to construct a canonical 109-byte event signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignalEnvelope {
    pub nvr_id: [u8; 16],
    pub cloud_event_id: [u8; 16],
    pub issued_at_secs: u64,
    pub signing_key_version: u32,
}

/// Validation failure returned while parsing or verifying a canonical signal body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignalError {
    ExceedsPreParseCeiling,
    InvalidLength,
    UnsupportedVersion,
    InvalidKeyVersion,
    InvalidTimestamp,
    InvalidSignature,
    InvalidSigningKey,
}

impl fmt::Display for SignalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::ExceedsPreParseCeiling => "signal body exceeds 256-byte pre-parse ceiling",
            Self::InvalidLength => "signal body must be exactly 109 bytes",
            Self::UnsupportedVersion => "signal protocol version must be 1",
            Self::InvalidKeyVersion => "signal signing key version must be at least 1",
            Self::InvalidTimestamp => {
                "signal issued-at timestamp must be whole non-negative seconds"
            }
            Self::InvalidSignature => "signal Ed25519 signature verification failed",
            Self::InvalidSigningKey => "signal Ed25519 signing seed is invalid",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for SignalError {}

/// Derives an Ed25519 key pair from a 32-byte raw seed.
pub fn key_pair_from_seed(seed: &[u8; 32]) -> Result<Ed25519KeyPair, SignalError> {
    Ed25519KeyPair::from_seed_unchecked(seed).map_err(|_| SignalError::InvalidSigningKey)
}

/// Returns the 32-byte Ed25519 public key for a 32-byte raw seed.
pub fn public_key_from_seed(seed: &[u8; 32]) -> Result<[u8; 32], SignalError> {
    let key_pair = key_pair_from_seed(seed)?;
    let bytes = key_pair.public_key().as_ref();
    let mut public_key = [0_u8; 32];
    if bytes.len() != public_key.len() {
        return Err(SignalError::InvalidSigningKey);
    }
    public_key.copy_from_slice(bytes);
    Ok(public_key)
}

/// Converts a whole-second Unix timestamp in milliseconds to unsigned seconds.
///
/// Rejects negative timestamps and non-whole-second millisecond values.
pub const fn whole_seconds_from_ms(timestamp_ms: i64) -> Result<u64, SignalError> {
    if timestamp_ms < 0 || timestamp_ms % 1_000 != 0 {
        return Err(SignalError::InvalidTimestamp);
    }
    Ok((timestamp_ms / 1_000) as u64)
}

/// Encodes and signs one canonical 109-byte version 1 Keeppeek Service event signal.
pub fn sign_signal(
    envelope: SignalEnvelope,
    seed: &[u8; 32],
) -> Result<[u8; CANONICAL_BODY_BYTES], SignalError> {
    if envelope.signing_key_version == 0 {
        return Err(SignalError::InvalidKeyVersion);
    }
    let key_pair = key_pair_from_seed(seed)?;
    let unsigned = encode_unsigned_body(envelope);
    let transcript = signing_transcript(&unsigned);
    let signature = key_pair.sign(&transcript);
    let sig_bytes = signature.as_ref();
    assert_eq!(
        sig_bytes.len(),
        SIGNATURE_BYTES,
        "Ed25519 signature must be exactly 64 bytes"
    );

    let mut body = [0_u8; CANONICAL_BODY_BYTES];
    body[..UNSIGNED_BODY_BYTES].copy_from_slice(&unsigned);
    body[UNSIGNED_BODY_BYTES..].copy_from_slice(sig_bytes);
    Ok(body)
}

/// Parses the fixed 109-byte structure without checking the Ed25519 signature.
pub fn parse_unverified_signal(bytes: &[u8]) -> Result<VerifiedSignal, SignalError> {
    if bytes.len() > PRE_PARSE_CEILING_BYTES {
        return Err(SignalError::ExceedsPreParseCeiling);
    }
    if bytes.len() != CANONICAL_BODY_BYTES {
        return Err(SignalError::InvalidLength);
    }
    let version = bytes[0];
    if version != PROTOCOL_VERSION_V1 {
        return Err(SignalError::UnsupportedVersion);
    }
    let mut nvr_id = [0_u8; 16];
    nvr_id.copy_from_slice(&bytes[1..17]);
    let mut cloud_event_id = [0_u8; 16];
    cloud_event_id.copy_from_slice(&bytes[17..33]);
    let issued_at_secs = u64::from_be_bytes(
        bytes[33..41]
            .try_into()
            .expect("slice of length 8 must convert to array"),
    );
    let signing_key_version = u32::from_be_bytes(
        bytes[41..45]
            .try_into()
            .expect("slice of length 4 must convert to array"),
    );
    if signing_key_version == 0 {
        return Err(SignalError::InvalidKeyVersion);
    }
    let mut signature = [0_u8; SIGNATURE_BYTES];
    signature.copy_from_slice(&bytes[UNSIGNED_BODY_BYTES..CANONICAL_BODY_BYTES]);
    Ok(VerifiedSignal {
        version,
        nvr_id,
        cloud_event_id,
        issued_at_secs,
        signing_key_version,
        signature,
    })
}

/// Parses and verifies one canonical 109-byte signal against `expected_public_key`.
pub fn parse_and_verify_signal(
    bytes: &[u8],
    expected_public_key: &[u8; 32],
) -> Result<VerifiedSignal, SignalError> {
    let parsed = parse_unverified_signal(bytes)?;
    let mut unsigned = [0_u8; UNSIGNED_BODY_BYTES];
    unsigned.copy_from_slice(&bytes[..UNSIGNED_BODY_BYTES]);
    let transcript = signing_transcript(&unsigned);
    let public_key = UnparsedPublicKey::new(&ED25519, expected_public_key);
    public_key
        .verify(&transcript, &parsed.signature)
        .map_err(|_| SignalError::InvalidSignature)?;
    Ok(parsed)
}

fn encode_unsigned_body(envelope: SignalEnvelope) -> [u8; UNSIGNED_BODY_BYTES] {
    let mut unsigned = [0_u8; UNSIGNED_BODY_BYTES];
    unsigned[0] = PROTOCOL_VERSION_V1;
    unsigned[1..17].copy_from_slice(&envelope.nvr_id);
    unsigned[17..33].copy_from_slice(&envelope.cloud_event_id);
    unsigned[33..41].copy_from_slice(&envelope.issued_at_secs.to_be_bytes());
    unsigned[41..45].copy_from_slice(&envelope.signing_key_version.to_be_bytes());
    unsigned
}

fn signing_transcript(unsigned: &[u8; UNSIGNED_BODY_BYTES]) -> [u8; TRANSCRIPT_BYTES] {
    let mut transcript = [0_u8; TRANSCRIPT_BYTES];
    transcript[..DOMAIN_SEPARATOR.len()].copy_from_slice(DOMAIN_SEPARATOR);
    transcript[DOMAIN_SEPARATOR.len()..].copy_from_slice(unsigned);
    transcript
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXED_SEED: [u8; 32] = [
        0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec, 0x2c,
        0xc4, 0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03, 0x1c, 0xae,
        0x7f, 0x60,
    ];
    const FIXED_NVR_ID: [u8; 16] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
        0x10,
    ];
    const FIXED_EVENT_ID: [u8; 16] = [
        0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
        0x00,
    ];

    fn sample_envelope() -> SignalEnvelope {
        SignalEnvelope {
            nvr_id: FIXED_NVR_ID,
            cloud_event_id: FIXED_EVENT_ID,
            issued_at_secs: 1_756_728_000,
            signing_key_version: 7,
        }
    }

    #[test]
    fn encodes_exact_109_byte_layout_and_verifies_domain_separation() {
        let envelope = sample_envelope();
        let body = sign_signal(envelope, &FIXED_SEED).unwrap();
        assert_eq!(body.len(), 109);
        assert_eq!(body[0], 1);
        assert_eq!(&body[1..17], &FIXED_NVR_ID);
        assert_eq!(&body[17..33], &FIXED_EVENT_ID);
        assert_eq!(&body[33..41], &1_756_728_000_u64.to_be_bytes());
        assert_eq!(&body[41..45], &7_u32.to_be_bytes());

        let public_key = public_key_from_seed(&FIXED_SEED).unwrap();
        let verified = parse_and_verify_signal(&body, &public_key).unwrap();
        assert_eq!(verified.version, 1);
        assert_eq!(verified.nvr_id, FIXED_NVR_ID);
        assert_eq!(verified.cloud_event_id, FIXED_EVENT_ID);
        assert_eq!(verified.issued_at_secs, 1_756_728_000);
        assert_eq!(verified.signing_key_version, 7);

        // A signature over raw unsigned bytes without the domain separator must fail.
        let key_pair = key_pair_from_seed(&FIXED_SEED).unwrap();
        let raw_sig = key_pair.sign(&body[..UNSIGNED_BODY_BYTES]);
        let mut without_domain = body;
        without_domain[UNSIGNED_BODY_BYTES..].copy_from_slice(raw_sig.as_ref());
        assert_eq!(
            parse_and_verify_signal(&without_domain, &public_key),
            Err(SignalError::InvalidSignature)
        );
    }

    #[test]
    fn rejects_every_single_field_mutation_wrong_key_and_size_boundaries() {
        let body = sign_signal(sample_envelope(), &FIXED_SEED).unwrap();
        let public_key = public_key_from_seed(&FIXED_SEED).unwrap();

        for offset in [0, 1, 16, 17, 32, 33, 40, 41, 44, 45, 108] {
            let mut mutated = body;
            mutated[offset] ^= 0x01;
            assert!(
                parse_and_verify_signal(&mutated, &public_key).is_err(),
                "mutated byte at offset {offset} must fail verification"
            );
        }

        let wrong_seed = [0x42_u8; 32];
        let wrong_public = public_key_from_seed(&wrong_seed).unwrap();
        assert_eq!(
            parse_and_verify_signal(&body, &wrong_public),
            Err(SignalError::InvalidSignature)
        );

        assert_eq!(
            parse_and_verify_signal(&body[..108], &public_key),
            Err(SignalError::InvalidLength)
        );
        let mut too_long = Vec::from(body);
        too_long.push(0);
        assert_eq!(
            parse_and_verify_signal(&too_long, &public_key),
            Err(SignalError::InvalidLength)
        );
        assert_eq!(
            parse_and_verify_signal(&[0_u8; 257], &public_key),
            Err(SignalError::ExceedsPreParseCeiling)
        );

        let mut zero_version = sample_envelope();
        zero_version.signing_key_version = 0;
        assert_eq!(
            sign_signal(zero_version, &FIXED_SEED),
            Err(SignalError::InvalidKeyVersion)
        );

        assert_eq!(whole_seconds_from_ms(1_700_000_000_000), Ok(1_700_000_000));
        assert_eq!(
            whole_seconds_from_ms(1_700_000_000_001),
            Err(SignalError::InvalidTimestamp)
        );
        assert_eq!(
            whole_seconds_from_ms(-1_000),
            Err(SignalError::InvalidTimestamp)
        );
    }
}
