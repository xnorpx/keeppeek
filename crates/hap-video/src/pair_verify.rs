use crate::{
    crypto,
    record::SessionKeys,
    tlv8::{Tlv8Map, Tlv8Writer},
};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use std::{collections::VecDeque, error::Error as StdError, fmt};
use subtle::ConstantTimeEq;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

const IDENTIFIER: u8 = 0x01;
const PUBLIC_KEY: u8 = 0x03;
const ENCRYPTED_DATA: u8 = 0x05;
const STATE: u8 = 0x06;
const ERROR: u8 = 0x07;
const SIGNATURE: u8 = 0x0a;
const STATE_M1: u8 = 1;
const STATE_M2: u8 = 2;
const STATE_M3: u8 = 3;
const STATE_M4: u8 = 4;
const ERROR_AUTHENTICATION: u8 = 2;
const PAIR_VERIFY_SALT: &[u8] = b"Pair-Verify-Encrypt-Salt";
const PAIR_VERIFY_INFO: &[u8] = b"Pair-Verify-Encrypt-Info";
const NONCE_M2: &[u8; 8] = b"PV-Msg02";
const NONCE_M3: &[u8; 8] = b"PV-Msg03";
const MAX_IDENTIFIER_SIZE: usize = 256;
const MAX_PAIR_VERIFY_SIZE: usize = 16 * 1024;

/// Long-term identity presented by the HomeKit accessory.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct AccessoryIdentity {
    identifier: Vec<u8>,
    signing_seed: [u8; 32],
}

impl AccessoryIdentity {
    /// Creates an identity from a persistent identifier and Ed25519 seed.
    pub fn new(identifier: Vec<u8>, signing_seed: [u8; 32]) -> Result<Self, PairVerifyError> {
        validate_identifier(&identifier)?;
        Ok(Self {
            identifier,
            signing_seed,
        })
    }

    /// Returns the accessory's persistent pairing identifier.
    pub fn identifier(&self) -> &[u8] {
        &self.identifier
    }

    /// Returns the accessory Ed25519 long-term public key.
    pub fn public_key(&self) -> [u8; 32] {
        SigningKey::from_bytes(&self.signing_seed)
            .verifying_key()
            .to_bytes()
    }

    pub(crate) fn sign(&self, message: &[u8]) -> [u8; 64] {
        SigningKey::from_bytes(&self.signing_seed)
            .sign(message)
            .to_bytes()
    }
}

impl fmt::Debug for AccessoryIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccessoryIdentity")
            .field("identifier", &String::from_utf8_lossy(&self.identifier))
            .finish_non_exhaustive()
    }
}

/// Controller identity loaded after a pairing lookup action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerPairing {
    /// Persistent controller pairing identifier.
    pub identifier: Vec<u8>,
    /// Controller Ed25519 long-term public key.
    pub public_key: [u8; 32],
    /// Whether this controller can administer pairings.
    pub administrator: bool,
}

/// Current progress of one Pair Verify exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairVerifyState {
    /// Waiting for controller M1.
    AwaitingM1,
    /// Accessory M2 was emitted; waiting for controller M3.
    AwaitingM3,
    /// M3 was decrypted; persistent pairing data is needed.
    AwaitingPairing,
    /// Exchange ended successfully or with a protocol error.
    Complete,
}

/// Input applied to [`PairVerify`].
#[derive(Debug)]
pub enum PairVerifyInput<'a> {
    /// Controller M1 or M3 TLV8 message.
    Message(&'a [u8]),
    /// Result of loading the controller requested by
    /// [`PairVerifyOutput::PairingRequired`].
    Pairing(Option<&'a ControllerPairing>),
}

/// Output produced by [`PairVerify::poll_output`].
pub enum PairVerifyOutput {
    /// No output remains; the adapter may wait for another input.
    Idle,
    /// Plaintext pair-verify response to send on the HAP connection.
    Response(Vec<u8>),
    /// Persistent pairing data is required to authenticate M3.
    PairingRequired {
        /// Controller identifier decrypted from M3.
        identifier: Vec<u8>,
    },
    /// Send plaintext M4 before switching the connection to these keys.
    Verified {
        /// Plaintext M4 response.
        response: Vec<u8>,
        /// Directional encrypted-control keys.
        session_keys: SessionKeys,
        /// Authenticated controller metadata.
        controller: ControllerPairing,
    },
}

impl fmt::Debug for PairVerifyOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle => f.write_str("Idle"),
            Self::Response(response) => f
                .debug_tuple("Response")
                .field(&format_args!("{} bytes", response.len()))
                .finish(),
            Self::PairingRequired { identifier } => f
                .debug_struct("PairingRequired")
                .field("identifier", &String::from_utf8_lossy(identifier))
                .finish(),
            Self::Verified { controller, .. } => f
                .debug_struct("Verified")
                .field("controller", controller)
                .finish_non_exhaustive(),
        }
    }
}

#[derive(Zeroize, ZeroizeOnDrop)]
struct SessionCrypto {
    controller_ephemeral_public: [u8; 32],
    accessory_ephemeral_public: [u8; 32],
    shared_secret: [u8; 32],
    pair_verify_key: [u8; 32],
}

#[derive(Zeroize, ZeroizeOnDrop)]
struct PendingPairing {
    identifier: Vec<u8>,
    signature: [u8; 64],
}

/// Accessory-side Pair Verify M1-M4 state machine.
pub struct PairVerify<'a> {
    identity: &'a AccessoryIdentity,
    ephemeral_secret: Option<Zeroizing<[u8; 32]>>,
    state: PairVerifyState,
    crypto: Option<SessionCrypto>,
    pending_pairing: Option<PendingPairing>,
    outputs: VecDeque<PairVerifyOutput>,
}

impl<'a> PairVerify<'a> {
    /// Creates an exchange with caller-generated X25519 secret bytes.
    pub fn new(identity: &'a AccessoryIdentity, ephemeral_secret: [u8; 32]) -> Self {
        Self {
            identity,
            ephemeral_secret: Some(Zeroizing::new(ephemeral_secret)),
            state: PairVerifyState::AwaitingM1,
            crypto: None,
            pending_pairing: None,
            outputs: VecDeque::new(),
        }
    }

    /// Returns current exchange progress.
    pub const fn state(&self) -> PairVerifyState {
        self.state
    }

    /// Applies one controller message or pairing lookup result.
    pub fn handle_input(&mut self, input: PairVerifyInput<'_>) -> Result<(), PairVerifyError> {
        if !self.outputs.is_empty() {
            return Err(PairVerifyError::OutputNotDrained);
        }
        match input {
            PairVerifyInput::Message(message) => self.handle_message(message),
            PairVerifyInput::Pairing(pairing) => self.handle_pairing(pairing),
        }
    }

    /// Polls one output until [`PairVerifyOutput::Idle`] is returned.
    pub fn poll_output(&mut self) -> PairVerifyOutput {
        self.outputs.pop_front().unwrap_or(PairVerifyOutput::Idle)
    }

    fn handle_message(&mut self, message: &[u8]) -> Result<(), PairVerifyError> {
        if message.len() > MAX_PAIR_VERIFY_SIZE {
            return Err(PairVerifyError::MessageTooLarge(message.len()));
        }
        let map = Tlv8Map::parse_bounded(message, MAX_PAIR_VERIFY_SIZE)
            .map_err(|_| PairVerifyError::MalformedTlv)?;
        let message_state = required_u8(&map, STATE, "state")?;
        match (self.state, message_state) {
            (PairVerifyState::AwaitingM1, STATE_M1) => self.handle_m1(&map),
            (PairVerifyState::AwaitingM3, STATE_M3) => self.handle_m3(&map),
            _ => Err(PairVerifyError::InvalidState {
                expected: self.state,
                message_state,
            }),
        }
    }

    fn handle_m1(&mut self, map: &Tlv8Map) -> Result<(), PairVerifyError> {
        let controller_ephemeral_public = exact_array::<32>(
            required(map, PUBLIC_KEY, "controller ephemeral public key")?,
            "controller ephemeral public key",
        )?;
        let ephemeral_secret = self
            .ephemeral_secret
            .take()
            .ok_or(PairVerifyError::MissingEphemeralSecret)?;
        let secret = StaticSecret::from(*ephemeral_secret);
        let accessory_ephemeral_public = PublicKey::from(&secret).to_bytes();
        let shared_secret = secret
            .diffie_hellman(&PublicKey::from(controller_ephemeral_public))
            .to_bytes();
        if bool::from(shared_secret.ct_eq(&[0; 32])) {
            return Err(PairVerifyError::InvalidPublicKey);
        }
        let pair_verify_key = derive_pair_verify_key(&shared_secret)?;

        let mut signed = Vec::with_capacity(
            accessory_ephemeral_public.len()
                + self.identity.identifier().len()
                + controller_ephemeral_public.len(),
        );
        signed.extend_from_slice(&accessory_ephemeral_public);
        signed.extend_from_slice(self.identity.identifier());
        signed.extend_from_slice(&controller_ephemeral_public);
        let signature = self.identity.sign(&signed);

        let mut sub_tlv = Vec::new();
        let mut sub_writer = Tlv8Writer::new(&mut sub_tlv);
        sub_writer.push(IDENTIFIER, self.identity.identifier());
        sub_writer.push(SIGNATURE, &signature);
        let encrypted = seal(&pair_verify_key, NONCE_M2, &sub_tlv)?;

        let mut response = Vec::new();
        let mut writer = Tlv8Writer::new(&mut response);
        writer.push_u8(STATE, STATE_M2);
        writer.push(PUBLIC_KEY, &accessory_ephemeral_public);
        writer.push(ENCRYPTED_DATA, &encrypted);

        self.crypto = Some(SessionCrypto {
            controller_ephemeral_public,
            accessory_ephemeral_public,
            shared_secret,
            pair_verify_key,
        });
        self.state = PairVerifyState::AwaitingM3;
        self.outputs.push_back(PairVerifyOutput::Response(response));
        Ok(())
    }

    fn handle_m3(&mut self, map: &Tlv8Map) -> Result<(), PairVerifyError> {
        let encrypted = required(map, ENCRYPTED_DATA, "encrypted controller proof")?;
        let crypto = self
            .crypto
            .as_ref()
            .ok_or(PairVerifyError::MissingSessionCrypto)?;
        let plaintext = open(&crypto.pair_verify_key, NONCE_M3, encrypted)?;
        let sub_tlv = Tlv8Map::parse_bounded(&plaintext, MAX_PAIR_VERIFY_SIZE)
            .map_err(|_| PairVerifyError::MalformedTlv)?;
        let identifier = required(&sub_tlv, IDENTIFIER, "controller identifier")?.to_vec();
        validate_identifier(&identifier)?;
        let signature = exact_array::<64>(
            required(&sub_tlv, SIGNATURE, "controller signature")?,
            "controller signature",
        )?;

        self.pending_pairing = Some(PendingPairing {
            identifier: identifier.clone(),
            signature,
        });
        self.state = PairVerifyState::AwaitingPairing;
        self.outputs
            .push_back(PairVerifyOutput::PairingRequired { identifier });
        Ok(())
    }

    fn handle_pairing(
        &mut self,
        pairing: Option<&ControllerPairing>,
    ) -> Result<(), PairVerifyError> {
        if self.state != PairVerifyState::AwaitingPairing {
            return Err(PairVerifyError::PairingNotExpected(self.state));
        }
        let pending = self
            .pending_pairing
            .take()
            .ok_or(PairVerifyError::MissingPendingPairing)?;
        let Some(pairing) = pairing.filter(|pairing| pairing.identifier == pending.identifier)
        else {
            self.authentication_failed();
            return Ok(());
        };
        validate_identifier(&pairing.identifier)?;
        let crypto = self
            .crypto
            .take()
            .ok_or(PairVerifyError::MissingSessionCrypto)?;

        let mut signed = Vec::with_capacity(
            crypto.controller_ephemeral_public.len()
                + pairing.identifier.len()
                + crypto.accessory_ephemeral_public.len(),
        );
        signed.extend_from_slice(&crypto.controller_ephemeral_public);
        signed.extend_from_slice(&pairing.identifier);
        signed.extend_from_slice(&crypto.accessory_ephemeral_public);
        let verifying_key = match VerifyingKey::from_bytes(&pairing.public_key) {
            Ok(key) => key,
            Err(_) => {
                self.authentication_failed();
                return Ok(());
            }
        };
        let signature = Signature::from_bytes(&pending.signature);
        if verifying_key.verify_strict(&signed, &signature).is_err() {
            self.authentication_failed();
            return Ok(());
        }

        let session_keys = SessionKeys::derive(&crypto.shared_secret)
            .map_err(|_| PairVerifyError::KeyDerivation)?;
        let response = state_response(STATE_M4);
        self.state = PairVerifyState::Complete;
        self.outputs.push_back(PairVerifyOutput::Verified {
            response,
            session_keys,
            controller: pairing.clone(),
        });
        Ok(())
    }

    fn authentication_failed(&mut self) {
        let mut response = Vec::new();
        let mut writer = Tlv8Writer::new(&mut response);
        writer.push_u8(STATE, STATE_M4);
        writer.push_u8(ERROR, ERROR_AUTHENTICATION);
        self.crypto = None;
        self.state = PairVerifyState::Complete;
        self.outputs.push_back(PairVerifyOutput::Response(response));
    }
}

/// Pair Verify parsing, state, or cryptographic failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairVerifyError {
    OutputNotDrained,
    MessageTooLarge(usize),
    MalformedTlv,
    MissingField(&'static str),
    InvalidFieldLength {
        field: &'static str,
        expected: usize,
        actual: usize,
    },
    InvalidIdentifierLength(usize),
    InvalidState {
        expected: PairVerifyState,
        message_state: u8,
    },
    PairingNotExpected(PairVerifyState),
    MissingEphemeralSecret,
    MissingSessionCrypto,
    MissingPendingPairing,
    InvalidPublicKey,
    KeyDerivation,
    Encrypt,
    Authenticate,
}

impl fmt::Display for PairVerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutputNotDrained => f.write_str("pending outputs must be drained before input"),
            Self::MessageTooLarge(size) => write!(f, "pair-verify message has {size} bytes"),
            Self::MalformedTlv => f.write_str("malformed pair-verify TLV8"),
            Self::MissingField(field) => write!(f, "missing {field}"),
            Self::InvalidFieldLength {
                field,
                expected,
                actual,
            } => write!(f, "{field} has {actual} bytes; expected {expected}"),
            Self::InvalidIdentifierLength(length) => {
                write!(f, "pairing identifier has invalid length {length}")
            }
            Self::InvalidState {
                expected,
                message_state,
            } => write!(
                f,
                "pair-verify message state {message_state} is invalid while {expected:?}"
            ),
            Self::PairingNotExpected(state) => {
                write!(f, "pairing lookup result is invalid while {state:?}")
            }
            Self::MissingEphemeralSecret => f.write_str("ephemeral secret was already consumed"),
            Self::MissingSessionCrypto => f.write_str("pair-verify session crypto is missing"),
            Self::MissingPendingPairing => f.write_str("pending controller pairing is missing"),
            Self::InvalidPublicKey => f.write_str("controller X25519 public key is invalid"),
            Self::KeyDerivation => f.write_str("unable to derive pair-verify key"),
            Self::Encrypt => f.write_str("unable to encrypt pair-verify response"),
            Self::Authenticate => f.write_str("unable to authenticate pair-verify proof"),
        }
    }
}

impl StdError for PairVerifyError {}

fn required<'a>(
    map: &'a Tlv8Map,
    field_type: u8,
    field: &'static str,
) -> Result<&'a [u8], PairVerifyError> {
    map.get_unique(field_type)
        .map_err(|_| PairVerifyError::MalformedTlv)?
        .ok_or(PairVerifyError::MissingField(field))
}

fn required_u8(map: &Tlv8Map, field_type: u8, field: &'static str) -> Result<u8, PairVerifyError> {
    let value = required(map, field_type, field)?;
    let [value] = value else {
        return Err(PairVerifyError::InvalidFieldLength {
            field,
            expected: 1,
            actual: value.len(),
        });
    };
    Ok(*value)
}

fn exact_array<const N: usize>(
    value: &[u8],
    field: &'static str,
) -> Result<[u8; N], PairVerifyError> {
    value
        .try_into()
        .map_err(|_| PairVerifyError::InvalidFieldLength {
            field,
            expected: N,
            actual: value.len(),
        })
}

const fn validate_identifier(identifier: &[u8]) -> Result<(), PairVerifyError> {
    if identifier.is_empty() || identifier.len() > MAX_IDENTIFIER_SIZE {
        return Err(PairVerifyError::InvalidIdentifierLength(identifier.len()));
    }
    Ok(())
}

fn derive_pair_verify_key(shared_secret: &[u8; 32]) -> Result<[u8; 32], PairVerifyError> {
    crypto::derive(shared_secret, PAIR_VERIFY_SALT, PAIR_VERIFY_INFO)
        .map_err(|_| PairVerifyError::KeyDerivation)
}

fn seal(
    key: &[u8; 32],
    nonce_label: &[u8; 8],
    plaintext: &[u8],
) -> Result<Vec<u8>, PairVerifyError> {
    crypto::seal(key, &crypto::label_nonce(nonce_label), &[], plaintext)
        .map_err(|_| PairVerifyError::Encrypt)
}

fn open(
    key: &[u8; 32],
    nonce_label: &[u8; 8],
    ciphertext: &[u8],
) -> Result<Vec<u8>, PairVerifyError> {
    crypto::open(key, &crypto::label_nonce(nonce_label), &[], ciphertext)
        .map_err(|_| PairVerifyError::Authenticate)
}

fn state_response(state: u8) -> Vec<u8> {
    let mut response = Vec::new();
    Tlv8Writer::new(&mut response).push_u8(STATE, state);
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCESSORY_ID: &[u8] = b"11:22:33:44:55:66";
    const CONTROLLER_ID: &[u8] = b"controller-1";

    fn m1(controller_public: [u8; 32]) -> Vec<u8> {
        let mut message = Vec::new();
        let mut writer = Tlv8Writer::new(&mut message);
        writer.push_u8(STATE, STATE_M1);
        writer.push(PUBLIC_KEY, &controller_public);
        message
    }

    fn m3(
        controller_secret: [u8; 32],
        controller_signing_seed: [u8; 32],
        m2: &[u8],
    ) -> (Vec<u8>, ControllerPairing) {
        let map = Tlv8Map::parse(m2).unwrap();
        let accessory_public: [u8; 32] = map
            .get_unique(PUBLIC_KEY)
            .unwrap()
            .unwrap()
            .try_into()
            .unwrap();
        let controller_secret = StaticSecret::from(controller_secret);
        let controller_public = PublicKey::from(&controller_secret).to_bytes();
        let shared_secret = controller_secret
            .diffie_hellman(&PublicKey::from(accessory_public))
            .to_bytes();
        let pair_verify_key = derive_pair_verify_key(&shared_secret).unwrap();

        let accessory_proof = open(
            &pair_verify_key,
            NONCE_M2,
            map.get_unique(ENCRYPTED_DATA).unwrap().unwrap(),
        )
        .unwrap();
        let accessory_proof = Tlv8Map::parse(&accessory_proof).unwrap();
        let accessory_signature = Signature::from_bytes(
            &accessory_proof
                .get_unique(SIGNATURE)
                .unwrap()
                .unwrap()
                .try_into()
                .unwrap(),
        );
        let mut accessory_signed = Vec::new();
        accessory_signed.extend_from_slice(&accessory_public);
        accessory_signed.extend_from_slice(ACCESSORY_ID);
        accessory_signed.extend_from_slice(&controller_public);
        let accessory_ltpk = SigningKey::from_bytes(&[1; 32]).verifying_key();
        accessory_ltpk
            .verify_strict(&accessory_signed, &accessory_signature)
            .unwrap();

        let controller_signing = SigningKey::from_bytes(&controller_signing_seed);
        let mut controller_signed = Vec::new();
        controller_signed.extend_from_slice(&controller_public);
        controller_signed.extend_from_slice(CONTROLLER_ID);
        controller_signed.extend_from_slice(&accessory_public);
        let signature = controller_signing.sign(&controller_signed).to_bytes();
        let mut proof = Vec::new();
        let mut proof_writer = Tlv8Writer::new(&mut proof);
        proof_writer.push(IDENTIFIER, CONTROLLER_ID);
        proof_writer.push(SIGNATURE, &signature);
        let encrypted = seal(&pair_verify_key, NONCE_M3, &proof).unwrap();
        let mut message = Vec::new();
        let mut writer = Tlv8Writer::new(&mut message);
        writer.push_u8(STATE, STATE_M3);
        writer.push(ENCRYPTED_DATA, &encrypted);
        (
            message,
            ControllerPairing {
                identifier: CONTROLLER_ID.to_vec(),
                public_key: controller_signing.verifying_key().to_bytes(),
                administrator: true,
            },
        )
    }

    fn drain_response(pair_verify: &mut PairVerify<'_>) -> Vec<u8> {
        let PairVerifyOutput::Response(response) = pair_verify.poll_output() else {
            panic!("expected pair-verify response");
        };
        assert!(matches!(pair_verify.poll_output(), PairVerifyOutput::Idle));
        response
    }

    #[test]
    fn completes_accessory_pair_verify_without_io() {
        let identity = AccessoryIdentity::new(ACCESSORY_ID.to_vec(), [1; 32]).unwrap();
        let controller_secret = [2; 32];
        let controller_public = PublicKey::from(&StaticSecret::from(controller_secret)).to_bytes();
        let mut pair_verify = PairVerify::new(&identity, [3; 32]);

        let first_message = m1(controller_public);
        pair_verify
            .handle_input(PairVerifyInput::Message(&first_message))
            .unwrap();
        let second_message = drain_response(&mut pair_verify);
        let (third_message, pairing) = m3(controller_secret, [4; 32], &second_message);
        pair_verify
            .handle_input(PairVerifyInput::Message(&third_message))
            .unwrap();
        let PairVerifyOutput::PairingRequired { identifier } = pair_verify.poll_output() else {
            panic!("expected pairing lookup");
        };
        assert_eq!(identifier, CONTROLLER_ID);
        pair_verify
            .handle_input(PairVerifyInput::Pairing(Some(&pairing)))
            .unwrap();
        let PairVerifyOutput::Verified {
            response,
            session_keys: _,
            controller,
        } = pair_verify.poll_output()
        else {
            panic!("expected verified session");
        };
        assert_eq!(
            Tlv8Map::parse(&response).unwrap().get_u8(STATE).unwrap(),
            Some(STATE_M4)
        );
        assert_eq!(controller, pairing);
        assert_eq!(pair_verify.state(), PairVerifyState::Complete);
    }

    #[test]
    fn unknown_pairing_gets_authentication_error() {
        let identity = AccessoryIdentity::new(ACCESSORY_ID.to_vec(), [1; 32]).unwrap();
        let controller_secret = [2; 32];
        let controller_public = PublicKey::from(&StaticSecret::from(controller_secret)).to_bytes();
        let mut pair_verify = PairVerify::new(&identity, [3; 32]);
        let first_message = m1(controller_public);
        pair_verify
            .handle_input(PairVerifyInput::Message(&first_message))
            .unwrap();
        let second_message = drain_response(&mut pair_verify);
        let (third_message, _) = m3(controller_secret, [4; 32], &second_message);
        pair_verify
            .handle_input(PairVerifyInput::Message(&third_message))
            .unwrap();
        let _ = pair_verify.poll_output();
        pair_verify
            .handle_input(PairVerifyInput::Pairing(None))
            .unwrap();
        let response = drain_response(&mut pair_verify);
        let response = Tlv8Map::parse(&response).unwrap();
        assert_eq!(response.get_u8(STATE).unwrap(), Some(STATE_M4));
        assert_eq!(response.get_u8(ERROR).unwrap(), Some(ERROR_AUTHENTICATION));
    }

    #[test]
    fn rejects_noncontributory_x25519_key() {
        let identity = AccessoryIdentity::new(ACCESSORY_ID.to_vec(), [1; 32]).unwrap();
        let mut pair_verify = PairVerify::new(&identity, [3; 32]);
        let first_message = m1([0; 32]);

        assert_eq!(
            pair_verify.handle_input(PairVerifyInput::Message(&first_message)),
            Err(PairVerifyError::InvalidPublicKey)
        );
    }
}
