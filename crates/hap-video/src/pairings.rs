use crate::{
    pair_verify::ControllerPairing,
    tlv8::{Tlv8Map, Tlv8Writer},
};
use std::{collections::VecDeque, error::Error as StdError, fmt};

const METHOD: u8 = 0x00;
const IDENTIFIER: u8 = 0x01;
const PUBLIC_KEY: u8 = 0x03;
const STATE: u8 = 0x06;
const ERROR: u8 = 0x07;
const PERMISSIONS: u8 = 0x0b;
const METHOD_ADD: u8 = 3;
const METHOD_REMOVE: u8 = 4;
const METHOD_LIST: u8 = 5;
const STATE_M1: u8 = 1;
const STATE_M2: u8 = 2;
const ERROR_UNKNOWN: u8 = 1;
const ERROR_AUTHENTICATION: u8 = 2;
const ERROR_MAX_PEERS: u8 = 4;
const MAX_MESSAGE_SIZE: usize = 16 * 1024;
const MAX_IDENTIFIER_SIZE: usize = 256;
const MAX_LISTED_PAIRINGS: usize = 256;

/// Progress of one encrypted `/pairings` request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingsState {
    /// Waiting for a controller request.
    AwaitingRequest,
    /// Waiting for an add operation to complete.
    AwaitingAdd,
    /// Waiting for a remove operation to complete.
    AwaitingRemove,
    /// Waiting for the adapter to list stored pairings.
    AwaitingList,
    /// Request completed.
    Complete,
}

/// Result of a pairing add or remove persistence action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingsStoreResult {
    /// Requested change was stored.
    Stored,
    /// No additional controller can be added.
    MaxPeers,
    /// Persistence failed.
    Failed,
}

/// Input applied to [`Pairings`].
#[derive(Debug)]
pub enum PairingsInput<'a> {
    /// Decrypted TLV8 request body.
    Message(&'a [u8]),
    /// Result of an add/remove action.
    StoreResult(PairingsStoreResult),
    /// Result of a list action.
    PairingList(&'a [ControllerPairing]),
}

/// Output produced by [`Pairings::poll_output`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingsOutput {
    /// No output remains.
    Idle,
    /// Add or replace a controller pairing.
    AddPairing {
        /// Pairing to persist.
        pairing: ControllerPairing,
    },
    /// Remove a controller pairing by identifier.
    RemovePairing {
        /// Pairing identifier to remove.
        identifier: Vec<u8>,
    },
    /// Load all controller pairings.
    ListPairings,
    /// Encrypted-session plaintext response body.
    Response(Vec<u8>),
}

/// Sans-I/O HAP pairing administration state machine.
pub struct Pairings {
    requester_is_administrator: bool,
    state: PairingsState,
    outputs: VecDeque<PairingsOutput>,
}

impl Pairings {
    /// Creates a request handler scoped to the authenticated controller.
    pub const fn new(requester_is_administrator: bool) -> Self {
        Self {
            requester_is_administrator,
            state: PairingsState::AwaitingRequest,
            outputs: VecDeque::new(),
        }
    }

    /// Returns request progress.
    pub const fn state(&self) -> PairingsState {
        self.state
    }

    /// Applies one request or persistence result.
    pub fn handle_input(&mut self, input: PairingsInput<'_>) -> Result<(), PairingsError> {
        if !self.outputs.is_empty() {
            return Err(PairingsError::OutputNotDrained);
        }
        match input {
            PairingsInput::Message(message) => self.handle_message(message),
            PairingsInput::StoreResult(result) => self.handle_store_result(result),
            PairingsInput::PairingList(pairings) => self.handle_pairing_list(pairings),
        }
    }

    /// Polls one output until [`PairingsOutput::Idle`] is returned.
    pub fn poll_output(&mut self) -> PairingsOutput {
        self.outputs.pop_front().unwrap_or(PairingsOutput::Idle)
    }

    fn handle_message(&mut self, message: &[u8]) -> Result<(), PairingsError> {
        if self.state != PairingsState::AwaitingRequest {
            return Err(PairingsError::InputNotExpected(self.state));
        }
        if message.len() > MAX_MESSAGE_SIZE {
            return Err(PairingsError::MessageTooLarge(message.len()));
        }
        let map = Tlv8Map::parse_bounded(message, MAX_MESSAGE_SIZE)
            .map_err(|_| PairingsError::MalformedTlv)?;
        if required_u8(&map, STATE, "state")? != STATE_M1 {
            return Err(PairingsError::InvalidMessageState);
        }
        if !self.requester_is_administrator {
            self.finish_with_error(ERROR_AUTHENTICATION);
            return Ok(());
        }
        match required_u8(&map, METHOD, "method")? {
            METHOD_ADD => self.add_pairing(&map),
            METHOD_REMOVE => self.remove_pairing(&map),
            METHOD_LIST => {
                self.state = PairingsState::AwaitingList;
                self.outputs.push_back(PairingsOutput::ListPairings);
                Ok(())
            }
            method => Err(PairingsError::UnsupportedMethod(method)),
        }
    }

    fn add_pairing(&mut self, map: &Tlv8Map) -> Result<(), PairingsError> {
        let identifier = required(map, IDENTIFIER, "pairing identifier")?.to_vec();
        validate_identifier(&identifier)?;
        let public_key = exact_array::<32>(
            required(map, PUBLIC_KEY, "pairing public key")?,
            "pairing public key",
        )?;
        let administrator = match required_u8(map, PERMISSIONS, "pairing permissions")? {
            0 => false,
            1 => true,
            permissions => return Err(PairingsError::InvalidPermissions(permissions)),
        };
        self.state = PairingsState::AwaitingAdd;
        self.outputs.push_back(PairingsOutput::AddPairing {
            pairing: ControllerPairing {
                identifier,
                public_key,
                administrator,
            },
        });
        Ok(())
    }

    fn remove_pairing(&mut self, map: &Tlv8Map) -> Result<(), PairingsError> {
        let identifier = required(map, IDENTIFIER, "pairing identifier")?.to_vec();
        validate_identifier(&identifier)?;
        self.state = PairingsState::AwaitingRemove;
        self.outputs
            .push_back(PairingsOutput::RemovePairing { identifier });
        Ok(())
    }

    fn handle_store_result(&mut self, result: PairingsStoreResult) -> Result<(), PairingsError> {
        if !matches!(
            self.state,
            PairingsState::AwaitingAdd | PairingsState::AwaitingRemove
        ) {
            return Err(PairingsError::InputNotExpected(self.state));
        }
        match result {
            PairingsStoreResult::Stored => self.finish_with_response(success_response()),
            PairingsStoreResult::MaxPeers => self.finish_with_error(ERROR_MAX_PEERS),
            PairingsStoreResult::Failed => self.finish_with_error(ERROR_UNKNOWN),
        }
        Ok(())
    }

    fn handle_pairing_list(&mut self, pairings: &[ControllerPairing]) -> Result<(), PairingsError> {
        if self.state != PairingsState::AwaitingList {
            return Err(PairingsError::InputNotExpected(self.state));
        }
        if pairings.len() > MAX_LISTED_PAIRINGS {
            return Err(PairingsError::TooManyPairings(pairings.len()));
        }
        let mut response = Vec::new();
        let mut writer = Tlv8Writer::new(&mut response);
        writer.push_u8(STATE, STATE_M2);
        for (index, pairing) in pairings.iter().enumerate() {
            validate_identifier(&pairing.identifier)?;
            if index > 0 {
                writer.push_separator();
            }
            writer.push(IDENTIFIER, &pairing.identifier);
            writer.push(PUBLIC_KEY, &pairing.public_key);
            writer.push_u8(PERMISSIONS, u8::from(pairing.administrator));
        }
        self.finish_with_response(response);
        Ok(())
    }

    fn finish_with_error(&mut self, error: u8) {
        let mut response = success_response();
        Tlv8Writer::new(&mut response).push_u8(ERROR, error);
        self.finish_with_response(response);
    }

    fn finish_with_response(&mut self, response: Vec<u8>) {
        self.state = PairingsState::Complete;
        self.outputs.push_back(PairingsOutput::Response(response));
    }
}

/// Pairing administration parse or state failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingsError {
    OutputNotDrained,
    InputNotExpected(PairingsState),
    MessageTooLarge(usize),
    MalformedTlv,
    MissingField(&'static str),
    InvalidFieldLength {
        field: &'static str,
        expected: usize,
        actual: usize,
    },
    InvalidIdentifierLength(usize),
    InvalidMessageState,
    UnsupportedMethod(u8),
    InvalidPermissions(u8),
    TooManyPairings(usize),
}

impl fmt::Display for PairingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutputNotDrained => f.write_str("pending outputs must be drained before input"),
            Self::InputNotExpected(state) => write!(f, "input is invalid while {state:?}"),
            Self::MessageTooLarge(size) => write!(f, "pairings message has {size} bytes"),
            Self::MalformedTlv => f.write_str("malformed pairings TLV8"),
            Self::MissingField(field) => write!(f, "missing {field}"),
            Self::InvalidFieldLength {
                field,
                expected,
                actual,
            } => write!(f, "{field} has {actual} bytes; expected {expected}"),
            Self::InvalidIdentifierLength(length) => {
                write!(f, "pairing identifier has invalid length {length}")
            }
            Self::InvalidMessageState => f.write_str("pairings message is not M1"),
            Self::UnsupportedMethod(method) => write!(f, "unsupported pairings method {method}"),
            Self::InvalidPermissions(permissions) => {
                write!(f, "invalid pairing permissions {permissions}")
            }
            Self::TooManyPairings(count) => write!(f, "pairing list has {count} entries"),
        }
    }
}

impl StdError for PairingsError {}

fn success_response() -> Vec<u8> {
    let mut response = Vec::new();
    Tlv8Writer::new(&mut response).push_u8(STATE, STATE_M2);
    response
}

fn required<'a>(
    map: &'a Tlv8Map,
    field_type: u8,
    field: &'static str,
) -> Result<&'a [u8], PairingsError> {
    map.get_unique(field_type)
        .map_err(|_| PairingsError::MalformedTlv)?
        .ok_or(PairingsError::MissingField(field))
}

fn required_u8(map: &Tlv8Map, field_type: u8, field: &'static str) -> Result<u8, PairingsError> {
    let value = required(map, field_type, field)?;
    let [value] = value else {
        return Err(PairingsError::InvalidFieldLength {
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
) -> Result<[u8; N], PairingsError> {
    value
        .try_into()
        .map_err(|_| PairingsError::InvalidFieldLength {
            field,
            expected: N,
            actual: value.len(),
        })
}

const fn validate_identifier(identifier: &[u8]) -> Result<(), PairingsError> {
    if identifier.is_empty() || identifier.len() > MAX_IDENTIFIER_SIZE {
        return Err(PairingsError::InvalidIdentifierLength(identifier.len()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: u8, fields: &[(u8, &[u8])]) -> Vec<u8> {
        let mut request = Vec::new();
        let mut writer = Tlv8Writer::new(&mut request);
        writer.push_u8(STATE, STATE_M1);
        writer.push_u8(METHOD, method);
        for (field_type, value) in fields {
            writer.push(*field_type, value);
        }
        request
    }

    fn pairing(identifier: &[u8], key: u8, administrator: bool) -> ControllerPairing {
        ControllerPairing {
            identifier: identifier.to_vec(),
            public_key: [key; 32],
            administrator,
        }
    }

    #[test]
    fn add_pairing_waits_for_storage() {
        let identifier = b"controller-2";
        let public_key = [7; 32];
        let permissions = [1];
        let request = request(
            METHOD_ADD,
            &[
                (IDENTIFIER, identifier),
                (PUBLIC_KEY, &public_key),
                (PERMISSIONS, &permissions),
            ],
        );
        let mut pairings = Pairings::new(true);

        pairings
            .handle_input(PairingsInput::Message(&request))
            .unwrap();
        assert_eq!(
            pairings.poll_output(),
            PairingsOutput::AddPairing {
                pairing: pairing(identifier, 7, true),
            }
        );
        pairings
            .handle_input(PairingsInput::StoreResult(PairingsStoreResult::Stored))
            .unwrap();
        let PairingsOutput::Response(response) = pairings.poll_output() else {
            panic!("expected response");
        };
        assert_eq!(
            Tlv8Map::parse(&response).unwrap().get_u8(STATE).unwrap(),
            Some(STATE_M2)
        );
    }

    #[test]
    fn list_pairings_preserves_repeated_entries() {
        let request = request(METHOD_LIST, &[]);
        let mut pairings = Pairings::new(true);
        pairings
            .handle_input(PairingsInput::Message(&request))
            .unwrap();
        assert_eq!(pairings.poll_output(), PairingsOutput::ListPairings);
        pairings
            .handle_input(PairingsInput::PairingList(&[
                pairing(b"one", 1, true),
                pairing(b"two", 2, false),
            ]))
            .unwrap();
        let PairingsOutput::Response(response) = pairings.poll_output() else {
            panic!("expected list response");
        };
        let items = Tlv8Map::parse(&response).unwrap().items().to_vec();
        assert_eq!(
            items
                .iter()
                .filter(|(field_type, _)| *field_type == IDENTIFIER)
                .count(),
            2
        );
        assert!(
            items
                .iter()
                .any(|(field_type, value)| { *field_type == 0xff && value.is_empty() })
        );
    }

    #[test]
    fn non_administrator_is_rejected_without_storage_action() {
        let request = request(METHOD_LIST, &[]);
        let mut pairings = Pairings::new(false);
        pairings
            .handle_input(PairingsInput::Message(&request))
            .unwrap();
        let PairingsOutput::Response(response) = pairings.poll_output() else {
            panic!("expected authorization response");
        };
        let response = Tlv8Map::parse(&response).unwrap();
        assert_eq!(response.get_u8(ERROR).unwrap(), Some(ERROR_AUTHENTICATION));
        assert_eq!(pairings.state(), PairingsState::Complete);
    }
}
