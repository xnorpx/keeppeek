use crate::{
    crypto,
    pair_verify::{AccessoryIdentity, ControllerPairing},
    srp::{SrpError, SrpServer},
    tlv8::{Tlv8Map, Tlv8Writer},
};
use ed25519_dalek::{Signature, VerifyingKey};
use std::{collections::VecDeque, error::Error as StdError, fmt};
use zeroize::Zeroizing;

const METHOD: u8 = 0x00;
const IDENTIFIER: u8 = 0x01;
const SALT: u8 = 0x02;
const PUBLIC_KEY: u8 = 0x03;
const PROOF: u8 = 0x04;
const ENCRYPTED_DATA: u8 = 0x05;
const STATE: u8 = 0x06;
const ERROR: u8 = 0x07;
const SIGNATURE: u8 = 0x0a;
const METHOD_PAIR_SETUP: u8 = 0;
const STATE_M1: u8 = 1;
const STATE_M2: u8 = 2;
const STATE_M3: u8 = 3;
const STATE_M4: u8 = 4;
const STATE_M5: u8 = 5;
const STATE_M6: u8 = 6;
const ERROR_UNKNOWN: u8 = 1;
const ERROR_AUTHENTICATION: u8 = 2;
const ERROR_MAX_PEERS: u8 = 4;
const ERROR_UNAVAILABLE: u8 = 6;
const ENCRYPT_SALT: &[u8] = b"Pair-Setup-Encrypt-Salt";
const ENCRYPT_INFO: &[u8] = b"Pair-Setup-Encrypt-Info";
const CONTROLLER_SIGN_SALT: &[u8] = b"Pair-Setup-Controller-Sign-Salt";
const CONTROLLER_SIGN_INFO: &[u8] = b"Pair-Setup-Controller-Sign-Info";
const ACCESSORY_SIGN_SALT: &[u8] = b"Pair-Setup-Accessory-Sign-Salt";
const ACCESSORY_SIGN_INFO: &[u8] = b"Pair-Setup-Accessory-Sign-Info";
const NONCE_M5: &[u8; 8] = b"PS-Msg05";
const NONCE_M6: &[u8; 8] = b"PS-Msg06";
const MAX_PAIR_SETUP_SIZE: usize = 16 * 1024;
const MAX_IDENTIFIER_SIZE: usize = 256;

/// Current progress of one Pair Setup exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairSetupState {
    /// Waiting for controller M1.
    AwaitingM1,
    /// Accessory M2 was emitted; waiting for controller M3.
    AwaitingM3,
    /// Accessory M4 was emitted; waiting for controller M5.
    AwaitingM5,
    /// The controller was authenticated; durable storage confirmation is needed.
    AwaitingPairingStore,
    /// Exchange ended successfully or with a protocol error.
    Complete,
}

/// Result of the adapter's persistent pairing write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingStoreResult {
    /// Pairing was durably stored.
    Stored,
    /// Accessory cannot accept another controller.
    MaxPeers,
    /// Storage failed for another reason.
    Failed,
}

/// Input applied to [`PairSetup`].
#[derive(Debug)]
pub enum PairSetupInput<'a> {
    /// Controller M1, M3, or M5 TLV8 message.
    Message(&'a [u8]),
    /// Result of the write requested by [`PairSetupOutput::StorePairing`].
    PairingStored(PairingStoreResult),
}

/// Output produced by [`PairSetup::poll_output`].
pub enum PairSetupOutput {
    /// No output remains; the adapter may wait for another input.
    Idle,
    /// Plaintext Pair Setup response to send on the HAP connection.
    Response(Vec<u8>),
    /// Persist this authenticated controller before completing Pair Setup.
    StorePairing {
        /// Controller pairing to store with administrator permissions.
        pairing: ControllerPairing,
    },
    /// Pair Setup completed; send M6 and expose the stored controller.
    Paired {
        /// Plaintext M6 response.
        response: Vec<u8>,
        /// Newly paired controller.
        controller: ControllerPairing,
    },
}

impl fmt::Debug for PairSetupOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle => f.write_str("Idle"),
            Self::Response(response) => f
                .debug_tuple("Response")
                .field(&format_args!("{} bytes", response.len()))
                .finish(),
            Self::StorePairing { pairing } => f
                .debug_struct("StorePairing")
                .field("pairing", pairing)
                .finish(),
            Self::Paired { controller, .. } => f
                .debug_struct("Paired")
                .field("controller", controller)
                .finish_non_exhaustive(),
        }
    }
}

/// Accessory-side Pair Setup M1-M6 state machine.
pub struct PairSetup<'a> {
    identity: &'a AccessoryIdentity,
    srp: Option<SrpServer>,
    already_paired: bool,
    state: PairSetupState,
    session_key: Option<Zeroizing<Vec<u8>>>,
    pending_pairing: Option<ControllerPairing>,
    outputs: VecDeque<PairSetupOutput>,
}

impl<'a> PairSetup<'a> {
    /// Creates a Pair Setup exchange with caller-generated salt and SRP secret.
    pub fn new(
        identity: &'a AccessoryIdentity,
        setup_code: &str,
        salt: [u8; 16],
        srp_private: [u8; 32],
        already_paired: bool,
    ) -> Result<Self, PairSetupError> {
        let setup_code = normalize_setup_code(setup_code)?;
        let srp =
            SrpServer::new(&setup_code, salt, srp_private).map_err(|_| PairSetupError::Srp)?;
        Ok(Self {
            identity,
            srp: Some(srp),
            already_paired,
            state: PairSetupState::AwaitingM1,
            session_key: None,
            pending_pairing: None,
            outputs: VecDeque::new(),
        })
    }

    /// Returns current exchange progress.
    pub const fn state(&self) -> PairSetupState {
        self.state
    }

    /// Applies one controller message or storage result.
    pub fn handle_input(&mut self, input: PairSetupInput<'_>) -> Result<(), PairSetupError> {
        if !self.outputs.is_empty() {
            return Err(PairSetupError::OutputNotDrained);
        }
        match input {
            PairSetupInput::Message(message) => self.handle_message(message),
            PairSetupInput::PairingStored(result) => self.handle_store_result(result),
        }
    }

    /// Polls one output until [`PairSetupOutput::Idle`] is returned.
    pub fn poll_output(&mut self) -> PairSetupOutput {
        self.outputs.pop_front().unwrap_or(PairSetupOutput::Idle)
    }

    fn handle_message(&mut self, message: &[u8]) -> Result<(), PairSetupError> {
        if message.len() > MAX_PAIR_SETUP_SIZE {
            return Err(PairSetupError::MessageTooLarge(message.len()));
        }
        let map = Tlv8Map::parse_bounded(message, MAX_PAIR_SETUP_SIZE)
            .map_err(|_| PairSetupError::MalformedTlv)?;
        let message_state = required_u8(&map, STATE, "state")?;
        match (self.state, message_state) {
            (PairSetupState::AwaitingM1, STATE_M1) => self.handle_m1(&map),
            (PairSetupState::AwaitingM3, STATE_M3) => self.handle_m3(&map),
            (PairSetupState::AwaitingM5, STATE_M5) => self.handle_m5(&map),
            _ => Err(PairSetupError::InvalidState {
                expected: self.state,
                message_state,
            }),
        }
    }

    fn handle_m1(&mut self, map: &Tlv8Map) -> Result<(), PairSetupError> {
        let method = required_u8(map, METHOD, "pairing method")?;
        if method != METHOD_PAIR_SETUP {
            return Err(PairSetupError::UnsupportedMethod(method));
        }
        if self.already_paired {
            self.srp = None;
            self.state = PairSetupState::Complete;
            self.outputs
                .push_back(PairSetupOutput::Response(error_response(
                    STATE_M2,
                    ERROR_UNAVAILABLE,
                )));
            return Ok(());
        }
        let srp = self.srp.as_ref().ok_or(PairSetupError::MissingSrp)?;
        let mut response = Vec::new();
        let mut writer = Tlv8Writer::new(&mut response);
        writer.push_u8(STATE, STATE_M2);
        writer.push(SALT, srp.salt());
        writer.push(PUBLIC_KEY, &srp.public_key());
        self.state = PairSetupState::AwaitingM3;
        self.outputs.push_back(PairSetupOutput::Response(response));
        Ok(())
    }

    fn handle_m3(&mut self, map: &Tlv8Map) -> Result<(), PairSetupError> {
        let controller_public = required(map, PUBLIC_KEY, "controller SRP public key")?;
        let controller_proof = required(map, PROOF, "controller SRP proof")?;
        let srp = self.srp.as_ref().ok_or(PairSetupError::MissingSrp)?;
        let verified = match srp.verify(controller_public, controller_proof) {
            Ok(verified) => verified,
            Err(SrpError::ProofMismatch | SrpError::InvalidControllerPublicKey) => {
                self.authentication_failed(STATE_M4);
                return Ok(());
            }
            Err(_) => return Err(PairSetupError::Srp),
        };
        self.srp = None;
        self.session_key = Some(verified.session_key);
        let mut response = Vec::new();
        let mut writer = Tlv8Writer::new(&mut response);
        writer.push_u8(STATE, STATE_M4);
        writer.push(PROOF, &verified.proof);
        self.state = PairSetupState::AwaitingM5;
        self.outputs.push_back(PairSetupOutput::Response(response));
        Ok(())
    }

    fn handle_m5(&mut self, map: &Tlv8Map) -> Result<(), PairSetupError> {
        let encrypted = required(map, ENCRYPTED_DATA, "encrypted controller pairing")?;
        let pairing = match self.authenticate_controller(encrypted) {
            Ok(pairing) => pairing,
            Err(PairSetupError::Authenticate | PairSetupError::MalformedTlv) => {
                self.authentication_failed(STATE_M6);
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        self.pending_pairing = Some(pairing.clone());
        self.state = PairSetupState::AwaitingPairingStore;
        self.outputs
            .push_back(PairSetupOutput::StorePairing { pairing });
        Ok(())
    }

    fn authenticate_controller(
        &self,
        encrypted: &[u8],
    ) -> Result<ControllerPairing, PairSetupError> {
        let session_key = self
            .session_key
            .as_ref()
            .ok_or(PairSetupError::MissingSessionKey)?;
        let encryption_key = Zeroizing::new(
            crypto::derive::<32>(session_key, ENCRYPT_SALT, ENCRYPT_INFO)
                .map_err(|_| PairSetupError::KeyDerivation)?,
        );
        let plaintext = crypto::open(
            &encryption_key,
            &crypto::label_nonce(NONCE_M5),
            &[],
            encrypted,
        )
        .map_err(|_| PairSetupError::Authenticate)?;
        let sub_tlv = Tlv8Map::parse_bounded(&plaintext, MAX_PAIR_SETUP_SIZE)
            .map_err(|_| PairSetupError::MalformedTlv)?;
        let identifier = required(&sub_tlv, IDENTIFIER, "controller identifier")?.to_vec();
        validate_identifier(&identifier)?;
        let public_key = exact_array::<32>(
            required(&sub_tlv, PUBLIC_KEY, "controller public key")?,
            "controller public key",
        )?;
        let signature = exact_array::<64>(
            required(&sub_tlv, SIGNATURE, "controller signature")?,
            "controller signature",
        )?;
        let controller_x = Zeroizing::new(
            crypto::derive::<32>(session_key, CONTROLLER_SIGN_SALT, CONTROLLER_SIGN_INFO)
                .map_err(|_| PairSetupError::KeyDerivation)?,
        );
        let mut signed =
            Vec::with_capacity(controller_x.len() + identifier.len() + public_key.len());
        signed.extend_from_slice(&*controller_x);
        signed.extend_from_slice(&identifier);
        signed.extend_from_slice(&public_key);
        let verifying_key =
            VerifyingKey::from_bytes(&public_key).map_err(|_| PairSetupError::Authenticate)?;
        if verifying_key
            .verify_strict(&signed, &Signature::from_bytes(&signature))
            .is_err()
        {
            return Err(PairSetupError::Authenticate);
        }
        Ok(ControllerPairing {
            identifier,
            public_key,
            administrator: true,
        })
    }

    fn handle_store_result(&mut self, result: PairingStoreResult) -> Result<(), PairSetupError> {
        if self.state != PairSetupState::AwaitingPairingStore {
            return Err(PairSetupError::StoreResultNotExpected(self.state));
        }
        match result {
            PairingStoreResult::Stored => self.finish_pairing(),
            PairingStoreResult::MaxPeers => {
                self.storage_failed(ERROR_MAX_PEERS);
                Ok(())
            }
            PairingStoreResult::Failed => {
                self.storage_failed(ERROR_UNKNOWN);
                Ok(())
            }
        }
    }

    fn finish_pairing(&mut self) -> Result<(), PairSetupError> {
        let controller = self
            .pending_pairing
            .take()
            .ok_or(PairSetupError::MissingPendingPairing)?;
        let session_key = self
            .session_key
            .take()
            .ok_or(PairSetupError::MissingSessionKey)?;
        let accessory_x = Zeroizing::new(
            crypto::derive::<32>(&session_key, ACCESSORY_SIGN_SALT, ACCESSORY_SIGN_INFO)
                .map_err(|_| PairSetupError::KeyDerivation)?,
        );
        let public_key = self.identity.public_key();
        let mut signed = Vec::with_capacity(
            accessory_x.len() + self.identity.identifier().len() + public_key.len(),
        );
        signed.extend_from_slice(&*accessory_x);
        signed.extend_from_slice(self.identity.identifier());
        signed.extend_from_slice(&public_key);
        let signature = self.identity.sign(&signed);
        let mut sub_tlv = Vec::new();
        let mut sub_writer = Tlv8Writer::new(&mut sub_tlv);
        sub_writer.push(IDENTIFIER, self.identity.identifier());
        sub_writer.push(PUBLIC_KEY, &public_key);
        sub_writer.push(SIGNATURE, &signature);
        let encryption_key = Zeroizing::new(
            crypto::derive::<32>(&session_key, ENCRYPT_SALT, ENCRYPT_INFO)
                .map_err(|_| PairSetupError::KeyDerivation)?,
        );
        let encrypted = crypto::seal(
            &encryption_key,
            &crypto::label_nonce(NONCE_M6),
            &[],
            &sub_tlv,
        )
        .map_err(|_| PairSetupError::Encrypt)?;
        let mut response = Vec::new();
        let mut writer = Tlv8Writer::new(&mut response);
        writer.push_u8(STATE, STATE_M6);
        writer.push(ENCRYPTED_DATA, &encrypted);
        self.state = PairSetupState::Complete;
        self.outputs.push_back(PairSetupOutput::Paired {
            response,
            controller,
        });
        Ok(())
    }

    fn authentication_failed(&mut self, response_state: u8) {
        self.srp = None;
        self.session_key = None;
        self.pending_pairing = None;
        self.state = PairSetupState::Complete;
        self.outputs
            .push_back(PairSetupOutput::Response(error_response(
                response_state,
                ERROR_AUTHENTICATION,
            )));
    }

    fn storage_failed(&mut self, error: u8) {
        self.session_key = None;
        self.pending_pairing = None;
        self.state = PairSetupState::Complete;
        self.outputs
            .push_back(PairSetupOutput::Response(error_response(STATE_M6, error)));
    }
}

/// Pair Setup parsing, state, or cryptographic failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairSetupError {
    OutputNotDrained,
    MessageTooLarge(usize),
    InvalidSetupCode,
    MalformedTlv,
    MissingField(&'static str),
    InvalidFieldLength {
        field: &'static str,
        expected: usize,
        actual: usize,
    },
    InvalidIdentifierLength(usize),
    InvalidState {
        expected: PairSetupState,
        message_state: u8,
    },
    UnsupportedMethod(u8),
    StoreResultNotExpected(PairSetupState),
    MissingSrp,
    MissingSessionKey,
    MissingPendingPairing,
    Srp,
    KeyDerivation,
    Encrypt,
    Authenticate,
}

impl fmt::Display for PairSetupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutputNotDrained => f.write_str("pending outputs must be drained before input"),
            Self::MessageTooLarge(size) => write!(f, "pair-setup message has {size} bytes"),
            Self::InvalidSetupCode => f.write_str("setup code must contain exactly eight digits"),
            Self::MalformedTlv => f.write_str("malformed pair-setup TLV8"),
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
                "pair-setup message state {message_state} is invalid while {expected:?}"
            ),
            Self::UnsupportedMethod(method) => write!(f, "unsupported pairing method {method}"),
            Self::StoreResultNotExpected(state) => {
                write!(f, "pairing storage result is invalid while {state:?}")
            }
            Self::MissingSrp => f.write_str("pair-setup SRP state is missing"),
            Self::MissingSessionKey => f.write_str("pair-setup session key is missing"),
            Self::MissingPendingPairing => f.write_str("pending pairing is missing"),
            Self::Srp => f.write_str("SRP parameter or scrambling failure"),
            Self::KeyDerivation => f.write_str("unable to derive pair-setup key"),
            Self::Encrypt => f.write_str("unable to encrypt pair-setup response"),
            Self::Authenticate => f.write_str("unable to authenticate pair-setup proof"),
        }
    }
}

impl StdError for PairSetupError {}

fn normalize_setup_code(setup_code: &str) -> Result<Zeroizing<Vec<u8>>, PairSetupError> {
    let digits: Vec<u8> = setup_code.bytes().filter(u8::is_ascii_digit).collect();
    if digits.len() != 8 {
        return Err(PairSetupError::InvalidSetupCode);
    }
    let mut normalized = Vec::with_capacity(10);
    normalized.extend_from_slice(&digits[..3]);
    normalized.push(b'-');
    normalized.extend_from_slice(&digits[3..5]);
    normalized.push(b'-');
    normalized.extend_from_slice(&digits[5..]);
    Ok(Zeroizing::new(normalized))
}

fn required<'a>(
    map: &'a Tlv8Map,
    field_type: u8,
    field: &'static str,
) -> Result<&'a [u8], PairSetupError> {
    map.get_unique(field_type)
        .map_err(|_| PairSetupError::MalformedTlv)?
        .ok_or(PairSetupError::MissingField(field))
}

fn required_u8(map: &Tlv8Map, field_type: u8, field: &'static str) -> Result<u8, PairSetupError> {
    let value = required(map, field_type, field)?;
    let [value] = value else {
        return Err(PairSetupError::InvalidFieldLength {
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
) -> Result<[u8; N], PairSetupError> {
    value
        .try_into()
        .map_err(|_| PairSetupError::InvalidFieldLength {
            field,
            expected: N,
            actual: value.len(),
        })
}

const fn validate_identifier(identifier: &[u8]) -> Result<(), PairSetupError> {
    if identifier.is_empty() || identifier.len() > MAX_IDENTIFIER_SIZE {
        return Err(PairSetupError::InvalidIdentifierLength(identifier.len()));
    }
    Ok(())
}

fn error_response(state: u8, error: u8) -> Vec<u8> {
    let mut response = Vec::new();
    let mut writer = Tlv8Writer::new(&mut response);
    writer.push_u8(STATE, state);
    writer.push_u8(ERROR, error);
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::srp::{test_accessory_proof, test_controller_exchange};
    use ed25519_dalek::{Signer, SigningKey};

    const ACCESSORY_ID: &[u8] = b"11:22:33:44:55:66";
    const CONTROLLER_ID: &[u8] = b"controller-1";
    const SETUP_CODE: &str = "111-22-333";

    fn message(state: u8, fields: &[(u8, &[u8])]) -> Vec<u8> {
        let mut message = Vec::new();
        let mut writer = Tlv8Writer::new(&mut message);
        writer.push_u8(STATE, state);
        for (field_type, value) in fields {
            writer.push(*field_type, value);
        }
        message
    }

    fn m1() -> Vec<u8> {
        let method = [METHOD_PAIR_SETUP];
        message(STATE_M1, &[(METHOD, &method)])
    }

    fn drain_response(pair_setup: &mut PairSetup<'_>) -> Vec<u8> {
        let PairSetupOutput::Response(response) = pair_setup.poll_output() else {
            panic!("expected pair-setup response");
        };
        assert!(matches!(pair_setup.poll_output(), PairSetupOutput::Idle));
        response
    }

    fn reach_m5(
        pair_setup: &mut PairSetup<'_>,
        controller_signing: &SigningKey,
    ) -> (Vec<u8>, Vec<u8>, ControllerPairing) {
        pair_setup
            .handle_input(PairSetupInput::Message(&m1()))
            .unwrap();
        let m2 = drain_response(pair_setup);
        let m2 = Tlv8Map::parse(&m2).unwrap();
        let salt = m2.get_unique(SALT).unwrap().unwrap();
        let public = m2.get_unique(PUBLIC_KEY).unwrap().unwrap();
        let (controller_public, controller_proof, session_key) =
            test_controller_exchange(SETUP_CODE.as_bytes(), salt, &[0x37; 32], public);
        let m3 = message(
            STATE_M3,
            &[(PUBLIC_KEY, &controller_public), (PROOF, &controller_proof)],
        );
        pair_setup
            .handle_input(PairSetupInput::Message(&m3))
            .unwrap();
        let m4 = drain_response(pair_setup);
        let m4 = Tlv8Map::parse(&m4).unwrap();
        assert_eq!(
            m4.get_unique(PROOF).unwrap().unwrap(),
            test_accessory_proof(&controller_public, &controller_proof, &session_key)
        );

        let controller_x = Zeroizing::new(
            crypto::derive::<32>(&session_key, CONTROLLER_SIGN_SALT, CONTROLLER_SIGN_INFO).unwrap(),
        );
        let controller_public_key = controller_signing.verifying_key().to_bytes();
        let mut signed = Vec::new();
        signed.extend_from_slice(&*controller_x);
        signed.extend_from_slice(CONTROLLER_ID);
        signed.extend_from_slice(&controller_public_key);
        let signature = controller_signing.sign(&signed).to_bytes();
        let mut sub_tlv = Vec::new();
        let mut sub_writer = Tlv8Writer::new(&mut sub_tlv);
        sub_writer.push(IDENTIFIER, CONTROLLER_ID);
        sub_writer.push(PUBLIC_KEY, &controller_public_key);
        sub_writer.push(SIGNATURE, &signature);
        let encryption_key =
            Zeroizing::new(crypto::derive::<32>(&session_key, ENCRYPT_SALT, ENCRYPT_INFO).unwrap());
        let encrypted = crypto::seal(
            &encryption_key,
            &crypto::label_nonce(NONCE_M5),
            &[],
            &sub_tlv,
        )
        .unwrap();
        (
            message(STATE_M5, &[(ENCRYPTED_DATA, &encrypted)]),
            session_key,
            ControllerPairing {
                identifier: CONTROLLER_ID.to_vec(),
                public_key: controller_public_key,
                administrator: true,
            },
        )
    }

    #[test]
    fn completes_pair_setup_only_after_storage_confirmation() {
        let identity = AccessoryIdentity::new(ACCESSORY_ID.to_vec(), [1; 32]).unwrap();
        let mut pair_setup =
            PairSetup::new(&identity, SETUP_CODE, [0x11; 16], [0x5a; 32], false).unwrap();
        let controller_signing = SigningKey::from_bytes(&[4; 32]);
        let (m5, session_key, expected_pairing) = reach_m5(&mut pair_setup, &controller_signing);
        pair_setup
            .handle_input(PairSetupInput::Message(&m5))
            .unwrap();
        let PairSetupOutput::StorePairing { pairing } = pair_setup.poll_output() else {
            panic!("expected persistent pairing request");
        };
        assert_eq!(pairing, expected_pairing);
        assert_eq!(pair_setup.state(), PairSetupState::AwaitingPairingStore);
        pair_setup
            .handle_input(PairSetupInput::PairingStored(PairingStoreResult::Stored))
            .unwrap();
        let PairSetupOutput::Paired {
            response,
            controller,
        } = pair_setup.poll_output()
        else {
            panic!("expected completed pairing");
        };
        assert_eq!(controller, expected_pairing);

        let response = Tlv8Map::parse(&response).unwrap();
        assert_eq!(response.get_u8(STATE).unwrap(), Some(STATE_M6));
        let encryption_key =
            Zeroizing::new(crypto::derive::<32>(&session_key, ENCRYPT_SALT, ENCRYPT_INFO).unwrap());
        let plaintext = crypto::open(
            &encryption_key,
            &crypto::label_nonce(NONCE_M6),
            &[],
            response.get_unique(ENCRYPTED_DATA).unwrap().unwrap(),
        )
        .unwrap();
        let accessory = Tlv8Map::parse(&plaintext).unwrap();
        assert_eq!(
            accessory.get_unique(IDENTIFIER).unwrap(),
            Some(ACCESSORY_ID)
        );
        assert_eq!(
            accessory.get_unique(PUBLIC_KEY).unwrap(),
            Some(identity.public_key().as_slice())
        );
        let signature: [u8; 64] = accessory
            .get_unique(SIGNATURE)
            .unwrap()
            .unwrap()
            .try_into()
            .unwrap();
        let accessory_x = Zeroizing::new(
            crypto::derive::<32>(&session_key, ACCESSORY_SIGN_SALT, ACCESSORY_SIGN_INFO).unwrap(),
        );
        let mut signed = Vec::new();
        signed.extend_from_slice(&*accessory_x);
        signed.extend_from_slice(ACCESSORY_ID);
        signed.extend_from_slice(&identity.public_key());
        VerifyingKey::from_bytes(&identity.public_key())
            .unwrap()
            .verify_strict(&signed, &Signature::from_bytes(&signature))
            .unwrap();
        assert_eq!(pair_setup.state(), PairSetupState::Complete);
    }

    #[test]
    fn paired_accessory_reports_unavailable() {
        let identity = AccessoryIdentity::new(ACCESSORY_ID.to_vec(), [1; 32]).unwrap();
        let mut pair_setup =
            PairSetup::new(&identity, SETUP_CODE, [0x11; 16], [0x5a; 32], true).unwrap();

        pair_setup
            .handle_input(PairSetupInput::Message(&m1()))
            .unwrap();
        let response = Tlv8Map::parse(&drain_response(&mut pair_setup)).unwrap();
        assert_eq!(response.get_u8(STATE).unwrap(), Some(STATE_M2));
        assert_eq!(response.get_u8(ERROR).unwrap(), Some(ERROR_UNAVAILABLE));
    }

    #[test]
    fn storage_capacity_failure_is_reported_in_m6() {
        let identity = AccessoryIdentity::new(ACCESSORY_ID.to_vec(), [1; 32]).unwrap();
        let mut pair_setup =
            PairSetup::new(&identity, SETUP_CODE, [0x11; 16], [0x5a; 32], false).unwrap();
        let controller_signing = SigningKey::from_bytes(&[4; 32]);
        let (m5, _, _) = reach_m5(&mut pair_setup, &controller_signing);
        pair_setup
            .handle_input(PairSetupInput::Message(&m5))
            .unwrap();
        let _ = pair_setup.poll_output();
        pair_setup
            .handle_input(PairSetupInput::PairingStored(PairingStoreResult::MaxPeers))
            .unwrap();
        let response = Tlv8Map::parse(&drain_response(&mut pair_setup)).unwrap();
        assert_eq!(response.get_u8(STATE).unwrap(), Some(STATE_M6));
        assert_eq!(response.get_u8(ERROR).unwrap(), Some(ERROR_MAX_PEERS));
    }
}
