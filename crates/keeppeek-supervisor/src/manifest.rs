//! Offline Ed25519 release manifest verification and streaming SHA-256 hashing.
//!
//! Release manifests bind a component, version, platform, schema compatibility
//! policy, binary filename, byte length, SHA-256 digest, and validity window
//! under a domain-separated Ed25519 signature verified with `aws-lc-rs`.

use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use aws_lc_rs::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::version::ReleaseVersion;

/// Supported schema version for signed release manifests.
pub const MANIFEST_SCHEMA_VERSION_V1: u32 = 1;

/// Maximum serialized manifest file size accepted from disk.
pub const MAX_MANIFEST_FILE_BYTES: u64 = 64 * 1024;

/// Maximum executable binary size accepted for staging (512 MiB).
pub const MAX_BINARY_SIZE_BYTES: u64 = 512 * 1024 * 1024;

/// Streaming buffer size used while computing SHA-256 digests.
const HASH_BUFFER_BYTES: usize = 64 * 1024;

/// Maximum accepted length of a signing key identifier.
const MAX_KEY_ID_BYTES: usize = 64;

/// Maximum accepted length of a platform identifier.
const MAX_PLATFORM_BYTES: usize = 64;

/// Maximum accepted length of an executable file name.
const MAX_BINARY_NAME_BYTES: usize = 128;

/// Maximum clock skew tolerated when checking `issued_at_unix_secs` (5 minutes).
pub const MAX_CLOCK_SKEW_SECS: u64 = 300;

/// Maximum lifetime allowed between `issued_at_unix_secs` and `expires_at_unix_secs` (365 days).
pub const MAX_MANIFEST_VALIDITY_SECS: u64 = 365 * 24 * 60 * 60;

/// Fixed domain separator prepended before Ed25519 signing and verification.
pub const MANIFEST_DOMAIN_SEPARATOR: &[u8] = b"keeppeek.release-manifest.v1\0";

const _: () = {
    assert!(MANIFEST_DOMAIN_SEPARATOR.len() == 29);
    assert!(MANIFEST_DOMAIN_SEPARATOR[MANIFEST_DOMAIN_SEPARATOR.len() - 1] == 0);
    assert!(HASH_BUFFER_BYTES > 0);
    assert!(MAX_BINARY_SIZE_BYTES > 0);
    assert!(MAX_MANIFEST_VALIDITY_SECS > MAX_CLOCK_SKEW_SECS);
};

/// Identifies a supervised KeepPeek executable component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ComponentKind {
    /// Main KeepPeek recorder and media gateway executable.
    #[serde(rename = "keeppeek")]
    Core,
    /// Separate KeepPeek Vision inference executable.
    #[serde(rename = "keeppeek-vision")]
    Vision,
}

impl ComponentKind {
    /// Returns the canonical component identifier string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Core => "keeppeek",
            Self::Vision => "keeppeek-vision",
        }
    }

    /// Returns the subdirectory name holding immutable slots for this component.
    #[must_use]
    pub const fn slot_dir_name(self) -> &'static str {
        match self {
            Self::Core => "versions",
            Self::Vision => "vision",
        }
    }

    /// Returns the expected executable filename for the given target OS.
    #[must_use]
    pub const fn binary_file_name_for_os(self, os: &str) -> &'static str {
        match (self, os.as_bytes()) {
            (Self::Core, b"windows") => "keeppeek.exe",
            (Self::Core, _) => "keeppeek",
            (Self::Vision, b"windows") => "keeppeek-vision.exe",
            (Self::Vision, _) => "keeppeek-vision",
        }
    }

    /// Returns the expected executable filename for the current host OS.
    #[must_use]
    pub const fn current_binary_file_name(self) -> &'static str {
        self.binary_file_name_for_os(std::env::consts::OS)
    }
}

impl fmt::Display for ComponentKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Declares whether a release maintains N-1 rollback compatibility with storage schemas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaCompatibility {
    /// Safe for unattended activation and automatic rollback to the previous binary.
    ExpandContract,
    /// Irreversible schema or config bump requiring explicit operator opt-in.
    Breaking,
}

impl SchemaCompatibility {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ExpandContract => "expand_contract",
            Self::Breaking => "breaking",
        }
    }
}

/// Unsigned metadata covered by the manifest Ed25519 signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifestPayload {
    /// Manifest format version (must be [`MANIFEST_SCHEMA_VERSION_V1`]).
    pub schema_version: u32,
    /// Supervised component targeted by this release.
    pub component: ComponentKind,
    /// Canonical semantic release version.
    pub version: ReleaseVersion,
    /// Target `<os>-<arch>` platform string (for example `windows-x86_64`).
    pub platform: String,
    /// Executable filename expected inside the immutable version directory.
    pub binary_name: String,
    /// Lowercase 64-character hexadecimal SHA-256 digest of the executable.
    pub sha256_hex: String,
    /// Exact byte length of the executable binary.
    pub size_bytes: u64,
    /// Storage and configuration rollback compatibility classification.
    pub schema_compatibility: SchemaCompatibility,
    /// Whole-second Unix timestamp when the manifest was signed.
    pub issued_at_unix_secs: u64,
    /// Whole-second Unix timestamp after which the manifest must be rejected.
    pub expires_at_unix_secs: u64,
    /// Identifier selecting the trusted offline Ed25519 verification key.
    pub signing_key_id: String,
}

/// Signed release manifest distributed alongside a release binary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedReleaseManifest {
    /// Signed release metadata.
    pub payload: ReleaseManifestPayload,
    /// Lowercase 128-character hexadecimal Ed25519 signature over canonical transcript bytes.
    pub signature_hex: String,
}

/// Trusted offline Ed25519 verification key bound to a key identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedSigningKey {
    /// Key identifier matched against `payload.signing_key_id`.
    pub key_id: String,
    /// Raw 32-byte Ed25519 public key.
    pub public_key_bytes: [u8; 32],
}

/// Validation or cryptographic failure while verifying a release manifest or binary.
#[derive(Debug)]
pub enum ManifestError {
    /// Manifest schema version is unsupported.
    UnsupportedSchemaVersion(u32),
    /// A string field was empty, oversized, or contained unsafe characters.
    InvalidField { field: &'static str, detail: String },
    /// Manifest timestamps are inverted or expired relative to the verification clock.
    ExpiredOrInvalidWindow {
        issued_at_unix_secs: u64,
        expires_at_unix_secs: u64,
        now_unix_secs: u64,
    },
    /// Manifest platform did not match the expected host platform.
    PlatformMismatch { expected: String, actual: String },
    /// Binary filename did not match the expected component filename for the platform.
    BinaryNameMismatch { expected: String, actual: String },
    /// Binary size was zero or exceeded [`MAX_BINARY_SIZE_BYTES`].
    InvalidBinarySize { size_bytes: u64 },
    /// No trusted signing key matched `signing_key_id`.
    UnknownSigningKey(String),
    /// Hex encoding for key, digest, or signature was malformed.
    InvalidHex { field: &'static str },
    /// Ed25519 signing seed was rejected by `aws-lc-rs`.
    InvalidSigningSeed,
    /// Ed25519 signature verification failed.
    InvalidSignature,
    /// Staged binary byte length did not match the signed manifest.
    SizeMismatch {
        expected_bytes: u64,
        actual_bytes: u64,
    },
    /// Staged binary SHA-256 digest did not match the signed manifest.
    DigestMismatch { expected: String, actual: String },
    /// Manifest file exceeded [`MAX_MANIFEST_FILE_BYTES`].
    ManifestTooLarge { size_bytes: u64 },
    /// Manifest JSON parsing failed.
    ParseJson(serde_json::Error),
    /// Filesystem I/O failed while reading the manifest or binary.
    Io(std::io::Error),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchemaVersion(version) => {
                write!(
                    formatter,
                    "unsupported release manifest schema version {version}"
                )
            }
            Self::InvalidField { field, detail } => {
                write!(formatter, "invalid manifest field `{field}`: {detail}")
            }
            Self::ExpiredOrInvalidWindow {
                issued_at_unix_secs,
                expires_at_unix_secs,
                now_unix_secs,
            } => write!(
                formatter,
                "release manifest window [{issued_at_unix_secs}, {expires_at_unix_secs}] is invalid at {now_unix_secs}"
            ),
            Self::PlatformMismatch { expected, actual } => write!(
                formatter,
                "release manifest platform `{actual}` does not match host `{expected}`"
            ),
            Self::BinaryNameMismatch { expected, actual } => write!(
                formatter,
                "release manifest binary_name `{actual}` does not match expected `{expected}`"
            ),
            Self::InvalidBinarySize { size_bytes } => write!(
                formatter,
                "release binary size {size_bytes} bytes is outside 1..={MAX_BINARY_SIZE_BYTES}"
            ),
            Self::UnknownSigningKey(key_id) => {
                write!(formatter, "untrusted release signing key id `{key_id}`")
            }
            Self::InvalidHex { field } => {
                write!(formatter, "invalid hexadecimal encoding in `{field}`")
            }
            Self::InvalidSigningSeed => formatter.write_str("invalid Ed25519 signing key seed"),
            Self::InvalidSignature => {
                formatter.write_str("release manifest Ed25519 signature verification failed")
            }
            Self::SizeMismatch {
                expected_bytes,
                actual_bytes,
            } => write!(
                formatter,
                "release binary length {actual_bytes} bytes does not match manifest {expected_bytes} bytes"
            ),
            Self::DigestMismatch { expected, actual } => write!(
                formatter,
                "release binary SHA-256 `{actual}` does not match manifest `{expected}`"
            ),
            Self::ManifestTooLarge { size_bytes } => write!(
                formatter,
                "release manifest is {size_bytes} bytes, exceeding {MAX_MANIFEST_FILE_BYTES} bytes"
            ),
            Self::ParseJson(error) => write!(formatter, "invalid release manifest JSON: {error}"),
            Self::Io(error) => write!(formatter, "release verification I/O error: {error}"),
        }
    }
}

impl std::error::Error for ManifestError {}

/// Returns the canonical `<os>-<arch>` platform string for the current host.
#[must_use]
pub fn current_platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// Derives a raw 32-byte Ed25519 public key from a 32-byte seed.
///
/// # Errors
///
/// Returns [`ManifestError::InvalidSigningSeed`] if `aws-lc-rs` rejects the seed.
pub fn public_key_from_seed(seed: &[u8; 32]) -> Result<[u8; 32], ManifestError> {
    let key_pair =
        Ed25519KeyPair::from_seed_unchecked(seed).map_err(|_| ManifestError::InvalidSigningSeed)?;
    let raw = key_pair.public_key().as_ref();
    raw.try_into()
        .map_err(|_| ManifestError::InvalidSigningSeed)
}

/// Signs `payload` with the provided 32-byte Ed25519 seed.
///
/// # Errors
///
/// Returns a [`ManifestError`] when `payload` fails structural validation or
/// when `seed` cannot initialize an Ed25519 key pair.
pub fn sign_manifest(
    payload: ReleaseManifestPayload,
    seed: &[u8; 32],
) -> Result<SignedReleaseManifest, ManifestError> {
    validate_payload_structure(&payload)?;
    let key_pair =
        Ed25519KeyPair::from_seed_unchecked(seed).map_err(|_| ManifestError::InvalidSigningSeed)?;
    let transcript = canonical_transcript(&payload);
    let signature = key_pair.sign(&transcript);
    Ok(SignedReleaseManifest {
        payload,
        signature_hex: encode_hex(signature.as_ref()),
    })
}

/// Reads and parses a [`SignedReleaseManifest`] from disk with an explicit size bound.
///
/// # Errors
///
/// Returns [`ManifestError`] on I/O failure, oversized files, or invalid JSON.
pub fn read_manifest_file(path: &Path) -> Result<SignedReleaseManifest, ManifestError> {
    let file = File::open(path).map_err(ManifestError::Io)?;
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(ManifestError::Io)?;
    let read_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if read_len > MAX_MANIFEST_FILE_BYTES {
        return Err(ManifestError::ManifestTooLarge {
            size_bytes: read_len,
        });
    }
    serde_json::from_slice(&bytes).map_err(ManifestError::ParseJson)
}

/// Verifies manifest structure, platform, expiry window, and Ed25519 signature.
///
/// # Errors
///
/// Returns [`ManifestError`] when any structural, temporal, platform, key, or
/// cryptographic invariant fails.
pub fn verify_manifest(
    manifest: &SignedReleaseManifest,
    trusted_keys: &[TrustedSigningKey],
    expected_platform: &str,
    now_unix_secs: u64,
) -> Result<(), ManifestError> {
    let payload = &manifest.payload;
    validate_payload_structure(payload)?;
    if payload.platform != expected_platform {
        return Err(ManifestError::PlatformMismatch {
            expected: expected_platform.to_owned(),
            actual: payload.platform.clone(),
        });
    }
    let expected_os = expected_platform.split('-').next().unwrap_or("");
    let expected_binary = payload.component.binary_file_name_for_os(expected_os);
    if payload.binary_name != expected_binary {
        return Err(ManifestError::BinaryNameMismatch {
            expected: expected_binary.to_owned(),
            actual: payload.binary_name.clone(),
        });
    }
    let max_issued = now_unix_secs.saturating_add(MAX_CLOCK_SKEW_SECS);
    let validity_span = payload
        .expires_at_unix_secs
        .saturating_sub(payload.issued_at_unix_secs);
    if payload.issued_at_unix_secs > payload.expires_at_unix_secs
        || payload.issued_at_unix_secs > max_issued
        || now_unix_secs > payload.expires_at_unix_secs
        || validity_span > MAX_MANIFEST_VALIDITY_SECS
    {
        return Err(ManifestError::ExpiredOrInvalidWindow {
            issued_at_unix_secs: payload.issued_at_unix_secs,
            expires_at_unix_secs: payload.expires_at_unix_secs,
            now_unix_secs,
        });
    }
    let trusted = trusted_keys
        .iter()
        .find(|key| key.key_id == payload.signing_key_id)
        .ok_or_else(|| ManifestError::UnknownSigningKey(payload.signing_key_id.clone()))?;
    let sig_bytes = decode_hex_fixed::<64>(&manifest.signature_hex, "signature_hex")?;
    let transcript = canonical_transcript(payload);
    let verifier = UnparsedPublicKey::new(&ED25519, &trusted.public_key_bytes);
    verifier
        .verify(&transcript, &sig_bytes)
        .map_err(|_| ManifestError::InvalidSignature)
}

/// Computes the byte length and lowercase SHA-256 hex digest of `path` using streaming I/O.
///
/// # Errors
///
/// Returns [`ManifestError::Io`] on read failure or [`ManifestError::InvalidBinarySize`]
/// if the file is empty or exceeds [`MAX_BINARY_SIZE_BYTES`].
pub fn hash_binary_file(path: &Path) -> Result<(u64, String), ManifestError> {
    let mut file = File::open(path).map_err(ManifestError::Io)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; HASH_BUFFER_BYTES];
    let mut total_bytes: u64 = 0;

    loop {
        let read_count = file.read(&mut buffer).map_err(ManifestError::Io)?;
        if read_count == 0 {
            break;
        }
        total_bytes = total_bytes
            .checked_add(u64::try_from(read_count).unwrap_or(u64::MAX))
            .ok_or(ManifestError::InvalidBinarySize {
                size_bytes: u64::MAX,
            })?;
        if total_bytes > MAX_BINARY_SIZE_BYTES {
            return Err(ManifestError::InvalidBinarySize {
                size_bytes: total_bytes,
            });
        }
        hasher.update(&buffer[..read_count]);
    }
    if total_bytes == 0 {
        return Err(ManifestError::InvalidBinarySize { size_bytes: 0 });
    }
    let digest = hasher.finalize();
    Ok((total_bytes, encode_hex(&digest)))
}

/// Verifies a signed manifest and validates that `binary_path` matches its size and SHA-256.
///
/// # Errors
///
/// Returns [`ManifestError`] when either manifest verification or binary digest
/// verification fails.
pub fn verify_manifest_and_file(
    manifest: &SignedReleaseManifest,
    binary_path: &Path,
    trusted_keys: &[TrustedSigningKey],
    expected_platform: &str,
    now_unix_secs: u64,
) -> Result<(), ManifestError> {
    verify_manifest(manifest, trusted_keys, expected_platform, now_unix_secs)?;
    let (actual_bytes, actual_hex) = hash_binary_file(binary_path)?;
    if actual_bytes != manifest.payload.size_bytes {
        return Err(ManifestError::SizeMismatch {
            expected_bytes: manifest.payload.size_bytes,
            actual_bytes,
        });
    }
    if actual_hex != manifest.payload.sha256_hex {
        return Err(ManifestError::DigestMismatch {
            expected: manifest.payload.sha256_hex.clone(),
            actual: actual_hex,
        });
    }
    Ok(())
}

/// Decodes a 64-character hex Ed25519 public key into a 32-byte array.
///
/// # Errors
///
/// Returns [`ManifestError::InvalidHex`] if the input is not 64 hex characters.
pub fn decode_public_key_hex(hex_str: &str) -> Result<[u8; 32], ManifestError> {
    decode_hex_fixed::<32>(hex_str.trim(), "public_key_hex")
}

/// Encodes `bytes` as lowercase hexadecimal text.
#[must_use]
pub fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

fn validate_payload_structure(payload: &ReleaseManifestPayload) -> Result<(), ManifestError> {
    if payload.schema_version != MANIFEST_SCHEMA_VERSION_V1 {
        return Err(ManifestError::UnsupportedSchemaVersion(
            payload.schema_version,
        ));
    }
    validate_identifier("platform", &payload.platform, MAX_PLATFORM_BYTES)?;
    validate_safe_file_name(&payload.binary_name)?;
    validate_identifier("signing_key_id", &payload.signing_key_id, MAX_KEY_ID_BYTES)?;
    if payload.size_bytes == 0 || payload.size_bytes > MAX_BINARY_SIZE_BYTES {
        return Err(ManifestError::InvalidBinarySize {
            size_bytes: payload.size_bytes,
        });
    }
    let _ = decode_hex_fixed::<32>(&payload.sha256_hex, "sha256_hex")?;
    if payload
        .sha256_hex
        .bytes()
        .any(|byte| byte.is_ascii_uppercase())
    {
        return Err(ManifestError::InvalidField {
            field: "sha256_hex",
            detail: "digest must use lowercase hex".to_owned(),
        });
    }
    Ok(())
}

fn validate_identifier(
    field: &'static str,
    value: &str,
    max_bytes: usize,
) -> Result<(), ManifestError> {
    if value.is_empty() || value.len() > max_bytes {
        return Err(ManifestError::InvalidField {
            field,
            detail: format!("length must be between 1 and {max_bytes} bytes"),
        });
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(ManifestError::InvalidField {
            field,
            detail: "must contain only ASCII letters, digits, '-', '_', or '.'".to_owned(),
        });
    }
    Ok(())
}

fn validate_safe_file_name(name: &str) -> Result<(), ManifestError> {
    if name.is_empty() || name.len() > MAX_BINARY_NAME_BYTES {
        return Err(ManifestError::InvalidField {
            field: "binary_name",
            detail: format!("length must be between 1 and {MAX_BINARY_NAME_BYTES} bytes"),
        });
    }
    if name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        return Err(ManifestError::InvalidField {
            field: "binary_name",
            detail: "must be a single filename without path separators".to_owned(),
        });
    }
    validate_identifier("binary_name", name, MAX_BINARY_NAME_BYTES)
}

fn canonical_transcript(payload: &ReleaseManifestPayload) -> Vec<u8> {
    let mut transcript = Vec::with_capacity(256);
    transcript.extend_from_slice(MANIFEST_DOMAIN_SEPARATOR);
    push_field(&mut transcript, &payload.schema_version.to_string());
    push_field(&mut transcript, payload.component.as_str());
    push_field(&mut transcript, &payload.version.to_string());
    push_field(&mut transcript, &payload.platform);
    push_field(&mut transcript, &payload.binary_name);
    push_field(&mut transcript, &payload.sha256_hex);
    push_field(&mut transcript, &payload.size_bytes.to_string());
    push_field(&mut transcript, payload.schema_compatibility.as_str());
    push_field(&mut transcript, &payload.issued_at_unix_secs.to_string());
    push_field(&mut transcript, &payload.expires_at_unix_secs.to_string());
    push_field(&mut transcript, &payload.signing_key_id);
    transcript
}

fn push_field(buffer: &mut Vec<u8>, value: &str) {
    let length = u32::try_from(value.len()).expect("validated manifest field length fits u32");
    buffer.extend_from_slice(&length.to_be_bytes());
    buffer.extend_from_slice(value.as_bytes());
}

fn decode_hex_fixed<const N: usize>(
    hex_str: &str,
    field: &'static str,
) -> Result<[u8; N], ManifestError> {
    if hex_str.len() != N * 2 {
        return Err(ManifestError::InvalidHex { field });
    }
    let mut output = [0_u8; N];
    let bytes = hex_str.as_bytes();
    for index in 0..N {
        let high = hex_nibble(bytes[index * 2]).ok_or(ManifestError::InvalidHex { field })?;
        let low = hex_nibble(bytes[index * 2 + 1]).ok_or(ManifestError::InvalidHex { field })?;
        output[index] = (high << 4) | low;
    }
    Ok(output)
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
