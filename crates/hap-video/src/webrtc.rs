use crate::tlv8::{Error as Tlv8Error, Tlv8Map, Tlv8Writer};
use std::{collections::BTreeMap, error::Error as StdError, fmt};

const MAX_CHARACTERISTIC_BYTES: usize = 256 * 1024;
const MAX_SDP_BYTES: usize = 128 * 1024;
const MAX_CANDIDATE_BYTES: usize = 4 * 1024;
const MAX_CANDIDATES: usize = 64;

/// Per-camera capacity satisfying Apple's minimum of six simultaneous WebRTC sessions.
const CONCURRENT_SESSION_CAPACITY: usize = 6;

/// HAP request identifier supplied by the connection adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(pub u64);

/// Controller-visible UUID identifying one WebRTC session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId([u8; 16]);

impl SessionId {
    /// Creates a session identifier from externally generated random bytes.
    pub const fn new(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Returns the UUID bytes encoded in HAP TLV8 values.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// WebRTC characteristic handled by this state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Characteristic {
    /// Controller requests an accessory-generated SDP offer.
    SolicitOffer,
    /// Controller supplies the SDP answer to an accessory offer.
    ProvideAnswer,
    /// Controller ends an existing stream.
    StreamingControl,
    /// Controller supplies a new offer for an active session.
    Reoffer,
    /// Controller updates SFrame receive keys for an active session.
    UpdateSession,
}

impl Characteristic {
    /// Returns the characteristic UUID from Apple's 2026 compatibility guide.
    pub const fn uuid(self) -> &'static str {
        match self {
            Self::SolicitOffer => "00008053-0000-1000-8000-0026BB765291",
            Self::ProvideAnswer => "00008054-0000-1000-8000-0026BB765291",
            Self::StreamingControl => "00008056-0000-1000-8000-0026BB765291",
            Self::Reoffer => "00008058-0000-1000-8000-0026BB765291",
            Self::UpdateSession => "0000805C-0000-1000-8000-0026BB765291",
        }
    }
}

/// Options requested by the controller when soliciting an offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OfferOptions {
    /// Whether end-to-end SFrame media encryption was requested.
    pub sframe_enabled: bool,
}

/// ICE candidate carried alongside SDP through HAP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IceCandidate {
    /// RFC 8825 candidate attribute value.
    pub candidate: String,
    /// Media stream identification tag, when associated with one media section.
    pub sdp_mid: Option<String>,
    /// Zero-based SDP media description index.
    pub sdp_mline_index: Option<u16>,
}

/// SDP and already-gathered ICE candidates returned by the WebRTC adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferDescription {
    /// RFC 8866-compliant SDP offer.
    pub sdp: String,
    /// Host or other candidates available when the HAP response is created.
    pub candidates: Vec<IceCandidate>,
}

/// Current state of one HomeKit WebRTC session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Waiting for the adapter to create an SDP offer.
    CreatingOffer,
    /// Offer was returned to HomeKit and the controller answer is pending.
    AwaitingAnswer,
    /// Adapter is applying the controller's answer.
    ApplyingAnswer,
    /// Answer was accepted and ICE/DTLS connection establishment is in progress.
    Connecting,
    /// WebRTC transport reported a connected session.
    Active,
    /// Adapter is applying a controller-generated renegotiation offer.
    Reoffering,
}

/// Status used by Provide Answer and Streaming Control responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum StreamingStatus {
    /// Command completed successfully.
    Success = 0,
    /// Session identifier is not known.
    UnknownSession = 1,
    /// Session cannot accept the command in its current state.
    Busy = 2,
    /// Command failed.
    Error = 3,
}

/// Input applied to the sans-I/O signaling state machine.
#[derive(Debug)]
pub enum Input<'a> {
    /// Raw WebRTC Solicit Offer characteristic write.
    SolicitOffer {
        /// HAP adapter request identifier.
        request_id: RequestId,
        /// Random session UUID generated outside the protocol core.
        session_id: SessionId,
        /// TLV8 characteristic value.
        value: &'a [u8],
    },
    /// Result of creating an offer in the WebRTC adapter.
    OfferCreated {
        /// Request that caused offer creation.
        request_id: RequestId,
        /// Session receiving the result.
        session_id: SessionId,
        /// Offer on success, or `None` on transport failure.
        offer: Option<OfferDescription>,
    },
    /// Raw WebRTC Provide Answer characteristic write.
    ProvideAnswer {
        /// HAP adapter request identifier.
        request_id: RequestId,
        /// TLV8 characteristic value.
        value: &'a [u8],
    },
    /// Result of applying the controller's SDP answer in the WebRTC adapter.
    AnswerApplied {
        /// Request that supplied the answer.
        request_id: RequestId,
        /// Session receiving the result.
        session_id: SessionId,
        /// Whether str0m accepted the answer and candidates.
        success: bool,
    },
    /// WebRTC adapter reported that ICE and DTLS connected.
    TransportConnected {
        /// Connected session.
        session_id: SessionId,
    },
    /// WebRTC adapter reported terminal transport closure.
    TransportClosed {
        /// Closed session.
        session_id: SessionId,
    },
    /// Raw WebRTC Streaming Control characteristic write.
    StreamingControl {
        /// HAP adapter request identifier.
        request_id: RequestId,
        /// TLV8 characteristic value.
        value: &'a [u8],
    },
    /// Raw WebRTC Reoffer characteristic write.
    Reoffer {
        /// HAP adapter request identifier.
        request_id: RequestId,
        /// TLV8 characteristic value.
        value: &'a [u8],
    },
    /// Result of accepting a controller-generated offer in the WebRTC adapter.
    ReofferAnswered {
        /// Request that supplied the reoffer.
        request_id: RequestId,
        /// Session receiving the result.
        session_id: SessionId,
        /// RFC 8866 SDP answer, or `None` when renegotiation failed.
        answer: Option<String>,
    },
    /// Raw WebRTC Update Session characteristic write.
    UpdateSession {
        /// HAP adapter request identifier.
        request_id: RequestId,
        /// TLV8 characteristic value.
        value: &'a [u8],
    },
}

/// Work requested from the separate WebRTC transport adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Create a send-only camera offer and gather candidates.
    CreateOffer {
        /// Originating HAP request.
        request_id: RequestId,
        /// New HomeKit session.
        session_id: SessionId,
        /// Controller offer options.
        options: OfferOptions,
    },
    /// Apply the controller answer and its additional ICE candidates.
    ApplyAnswer {
        /// Originating HAP request.
        request_id: RequestId,
        /// Existing HomeKit session.
        session_id: SessionId,
        /// RFC 8866-compliant SDP answer.
        sdp: String,
        /// Controller ICE candidates supplied with the answer.
        candidates: Vec<IceCandidate>,
    },
    /// Accept a controller-generated offer and produce an SDP answer.
    AcceptReoffer {
        /// Originating HAP request.
        request_id: RequestId,
        /// Existing HomeKit session.
        session_id: SessionId,
        /// RFC 8866-compliant SDP offer.
        sdp: String,
    },
    /// Tear down and release a WebRTC transport.
    EndSession {
        /// Session to release.
        session_id: SessionId,
    },
}

/// HAP characteristic write response ready for the connection adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteResponse {
    /// Request receiving this response.
    pub request_id: RequestId,
    /// Characteristic whose write response is encoded.
    pub characteristic: Characteristic,
    /// Raw TLV8 response value.
    pub value: Vec<u8>,
}

/// Observable signaling event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Number exposed by WebRTC Number of Active Sessions changed.
    ActiveSessionsChanged(u8),
}

/// Output produced by [`WebRtcDevice::poll_output`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    /// No queued output remains; the adapter may wait for its next input.
    Idle,
    /// Work for the WebRTC adapter.
    Action(Action),
    /// Write response for the HAP connection adapter.
    WriteResponse(WriteResponse),
    /// Observable state change.
    Event(Event),
}

#[derive(Debug, Clone, Copy)]
struct Session {
    state: SessionState,
    pending_request: RequestId,
}

/// Sans-I/O state for the WebRTC characteristics of one camera accessory.
pub struct WebRtcDevice {
    enabled: bool,
    sessions: BTreeMap<SessionId, Session>,
    outputs: Vec<Output>,
    output_offset: usize,
}

impl Default for WebRtcDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl WebRtcDevice {
    /// Creates an enabled camera service supporting the required six sessions.
    pub const fn new() -> Self {
        Self {
            enabled: true,
            sessions: BTreeMap::new(),
            outputs: Vec::new(),
            output_offset: 0,
        }
    }

    /// Enables or disables new HomeKit stream solicitation.
    pub const fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Returns the active-session characteristic value.
    pub fn active_session_count(&self) -> u8 {
        self.sessions
            .values()
            .filter(|session| {
                matches!(
                    session.state,
                    SessionState::Active | SessionState::Reoffering
                )
            })
            .count() as u8
    }

    /// Returns the current state of a HomeKit session.
    pub fn session_state(&self, session_id: SessionId) -> Option<SessionState> {
        self.sessions.get(&session_id).map(|session| session.state)
    }

    /// Returns a stable snapshot of currently known session identifiers.
    pub fn session_ids(&self) -> Vec<SessionId> {
        self.sessions.keys().copied().collect()
    }

    /// Applies one HAP or WebRTC transport input.
    pub fn handle_input(&mut self, input: Input<'_>) -> Result<(), Error> {
        if self.output_offset < self.outputs.len() {
            return Err(Error::OutputNotDrained);
        }
        self.outputs.clear();
        self.output_offset = 0;
        match input {
            Input::SolicitOffer {
                request_id,
                session_id,
                value,
            } => self.solicit_offer(request_id, session_id, value),
            Input::OfferCreated {
                request_id,
                session_id,
                offer,
            } => self.offer_created(request_id, session_id, offer),
            Input::ProvideAnswer { request_id, value } => self.provide_answer(request_id, value),
            Input::AnswerApplied {
                request_id,
                session_id,
                success,
            } => self.answer_applied(request_id, session_id, success),
            Input::TransportConnected { session_id } => self.transport_connected(session_id),
            Input::TransportClosed { session_id } => {
                self.remove_session(session_id);
                Ok(())
            }
            Input::StreamingControl { request_id, value } => {
                self.streaming_control(request_id, value)
            }
            Input::Reoffer { request_id, value } => self.reoffer(request_id, value),
            Input::ReofferAnswered {
                request_id,
                session_id,
                answer,
            } => self.reoffer_answered(request_id, session_id, answer),
            Input::UpdateSession { request_id, value } => self.update_session(request_id, value),
        }
    }

    /// Polls one output until [`Output::Idle`] is returned.
    pub fn poll_output(&mut self) -> Output {
        let Some(output) = self.outputs.get(self.output_offset).cloned() else {
            return Output::Idle;
        };
        self.output_offset += 1;
        output
    }

    fn solicit_offer(
        &mut self,
        request_id: RequestId,
        session_id: SessionId,
        value: &[u8],
    ) -> Result<(), Error> {
        let options = decode_offer_options(value)?;
        if self.sessions.contains_key(&session_id) {
            return Err(Error::DuplicateSession(session_id));
        }
        if !self.enabled {
            self.write_response(
                request_id,
                Characteristic::SolicitOffer,
                encode_solicit_response(session_id, SolicitStatus::PrivacyModeActive, None)?,
            );
            return Ok(());
        }
        if options.sframe_enabled || self.sessions.len() >= CONCURRENT_SESSION_CAPACITY {
            self.write_response(
                request_id,
                Characteristic::SolicitOffer,
                encode_solicit_response(session_id, SolicitStatus::Error, None)?,
            );
            return Ok(());
        }
        self.sessions.insert(
            session_id,
            Session {
                state: SessionState::CreatingOffer,
                pending_request: request_id,
            },
        );
        self.outputs.push(Output::Action(Action::CreateOffer {
            request_id,
            session_id,
            options,
        }));
        Ok(())
    }

    fn offer_created(
        &mut self,
        request_id: RequestId,
        session_id: SessionId,
        offer: Option<OfferDescription>,
    ) -> Result<(), Error> {
        self.require_pending(session_id, request_id, SessionState::CreatingOffer)?;
        let Some(offer) = offer else {
            self.sessions.remove(&session_id);
            self.write_response(
                request_id,
                Characteristic::SolicitOffer,
                encode_solicit_response(session_id, SolicitStatus::Error, None)?,
            );
            return Ok(());
        };
        validate_sdp_and_candidates(&offer.sdp, &offer.candidates)?;
        self.sessions.insert(
            session_id,
            Session {
                state: SessionState::AwaitingAnswer,
                pending_request: request_id,
            },
        );
        self.write_response(
            request_id,
            Characteristic::SolicitOffer,
            encode_solicit_response(session_id, SolicitStatus::Success, Some(&offer))?,
        );
        Ok(())
    }

    fn provide_answer(&mut self, request_id: RequestId, value: &[u8]) -> Result<(), Error> {
        let answer = decode_answer(value)?;
        let Some(session) = self.sessions.get_mut(&answer.session_id) else {
            self.write_response(
                request_id,
                Characteristic::ProvideAnswer,
                encode_status_response(answer.session_id, StreamingStatus::UnknownSession),
            );
            return Ok(());
        };
        if session.state != SessionState::AwaitingAnswer {
            self.write_response(
                request_id,
                Characteristic::ProvideAnswer,
                encode_status_response(answer.session_id, StreamingStatus::Busy),
            );
            return Ok(());
        }
        session.state = SessionState::ApplyingAnswer;
        session.pending_request = request_id;
        self.outputs.push(Output::Action(Action::ApplyAnswer {
            request_id,
            session_id: answer.session_id,
            sdp: answer.sdp,
            candidates: answer.candidates,
        }));
        Ok(())
    }

    fn answer_applied(
        &mut self,
        request_id: RequestId,
        session_id: SessionId,
        success: bool,
    ) -> Result<(), Error> {
        self.require_pending(session_id, request_id, SessionState::ApplyingAnswer)?;
        let status = if success {
            if let Some(session) = self.sessions.get_mut(&session_id) {
                session.state = SessionState::Connecting;
            }
            StreamingStatus::Success
        } else {
            self.sessions.remove(&session_id);
            StreamingStatus::Error
        };
        self.write_response(
            request_id,
            Characteristic::ProvideAnswer,
            encode_status_response(session_id, status),
        );
        Ok(())
    }

    fn transport_connected(&mut self, session_id: SessionId) -> Result<(), Error> {
        let previous_count = self.active_session_count();
        let Some(session) = self.sessions.get_mut(&session_id) else {
            return Err(Error::UnknownSession(session_id));
        };
        if session.state != SessionState::Connecting {
            return Err(Error::InvalidTransition {
                session_id,
                expected: SessionState::Connecting,
                actual: session.state,
            });
        }
        session.state = SessionState::Active;
        self.notify_active_count(previous_count);
        Ok(())
    }

    fn streaming_control(&mut self, request_id: RequestId, value: &[u8]) -> Result<(), Error> {
        let (session_id, command) = decode_streaming_control(value)?;
        if command != 1 {
            return Err(Error::UnsupportedCommand(command));
        }
        let previous_count = self.active_session_count();
        let status = if self.sessions.remove(&session_id).is_some() {
            self.outputs
                .push(Output::Action(Action::EndSession { session_id }));
            StreamingStatus::Success
        } else {
            StreamingStatus::UnknownSession
        };
        self.write_response(
            request_id,
            Characteristic::StreamingControl,
            encode_status_response(session_id, status),
        );
        self.notify_active_count(previous_count);
        Ok(())
    }

    fn reoffer(&mut self, request_id: RequestId, value: &[u8]) -> Result<(), Error> {
        let reoffer = decode_reoffer(value)?;
        let Some(session) = self.sessions.get_mut(&reoffer.session_id) else {
            self.write_response(
                request_id,
                Characteristic::Reoffer,
                encode_reoffer_response(reoffer.session_id, StreamingStatus::UnknownSession, None)?,
            );
            return Ok(());
        };
        if session.state != SessionState::Active {
            self.write_response(
                request_id,
                Characteristic::Reoffer,
                encode_reoffer_response(reoffer.session_id, StreamingStatus::Busy, None)?,
            );
            return Ok(());
        }
        if reoffer.options.sframe_enabled {
            self.write_response(
                request_id,
                Characteristic::Reoffer,
                encode_reoffer_response(reoffer.session_id, StreamingStatus::Error, None)?,
            );
            return Ok(());
        }
        session.state = SessionState::Reoffering;
        session.pending_request = request_id;
        self.outputs.push(Output::Action(Action::AcceptReoffer {
            request_id,
            session_id: reoffer.session_id,
            sdp: reoffer.sdp,
        }));
        Ok(())
    }

    fn reoffer_answered(
        &mut self,
        request_id: RequestId,
        session_id: SessionId,
        answer: Option<String>,
    ) -> Result<(), Error> {
        self.require_pending(session_id, request_id, SessionState::Reoffering)?;
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.state = SessionState::Active;
        }
        let (status, answer) = match answer {
            Some(answer) => {
                validate_sdp_and_candidates(&answer, &[])?;
                (StreamingStatus::Success, Some(answer))
            }
            None => (StreamingStatus::Error, None),
        };
        self.write_response(
            request_id,
            Characteristic::Reoffer,
            encode_reoffer_response(session_id, status, answer.as_deref())?,
        );
        Ok(())
    }

    fn update_session(&mut self, request_id: RequestId, value: &[u8]) -> Result<(), Error> {
        let map = parse_map(value)?;
        let session_id = decode_session_id(required(&map, 1, "session identifier")?)?;
        let status = if self.sessions.contains_key(&session_id) {
            StreamingStatus::Error
        } else {
            StreamingStatus::UnknownSession
        };
        self.write_response(
            request_id,
            Characteristic::UpdateSession,
            encode_status_response(session_id, status),
        );
        Ok(())
    }

    fn remove_session(&mut self, session_id: SessionId) {
        let previous_count = self.active_session_count();
        self.sessions.remove(&session_id);
        self.notify_active_count(previous_count);
    }

    fn require_pending(
        &self,
        session_id: SessionId,
        request_id: RequestId,
        expected: SessionState,
    ) -> Result<(), Error> {
        let session = self
            .sessions
            .get(&session_id)
            .ok_or(Error::UnknownSession(session_id))?;
        if session.pending_request != request_id {
            return Err(Error::MismatchedRequest {
                expected: session.pending_request,
                actual: request_id,
            });
        }
        if session.state != expected {
            return Err(Error::InvalidTransition {
                session_id,
                expected,
                actual: session.state,
            });
        }
        Ok(())
    }

    fn write_response(
        &mut self,
        request_id: RequestId,
        characteristic: Characteristic,
        value: Vec<u8>,
    ) {
        self.outputs.push(Output::WriteResponse(WriteResponse {
            request_id,
            characteristic,
            value,
        }));
    }

    fn notify_active_count(&mut self, previous_count: u8) {
        let active_count = self.active_session_count();
        if active_count != previous_count {
            self.outputs
                .push(Output::Event(Event::ActiveSessionsChanged(active_count)));
        }
    }
}

#[derive(Debug)]
struct Answer {
    session_id: SessionId,
    sdp: String,
    candidates: Vec<IceCandidate>,
}

#[derive(Debug)]
struct Reoffer {
    session_id: SessionId,
    sdp: String,
    options: OfferOptions,
}

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
enum SolicitStatus {
    Success = 0,
    PrivacyModeActive = 1,
    Error = 2,
}

fn decode_offer_options(value: &[u8]) -> Result<OfferOptions, Error> {
    let map = parse_map(value)?;
    let options = required(&map, 1, "offer options")?;
    decode_nested_offer_options(options)
}

fn decode_nested_offer_options(value: &[u8]) -> Result<OfferOptions, Error> {
    let options =
        Tlv8Map::parse_bounded(value, MAX_CHARACTERISTIC_BYTES).map_err(|_| Error::MalformedTlv)?;
    let sframe_enabled = required_u8(&options, 1, "SFrame enabled")?;
    let sframe_enabled = match sframe_enabled {
        0 => false,
        1 => true,
        value => return Err(Error::InvalidBoolean(value)),
    };
    Ok(OfferOptions { sframe_enabled })
}

fn decode_answer(value: &[u8]) -> Result<Answer, Error> {
    let map = parse_map(value)?;
    let session_id = decode_session_id(required(&map, 1, "session identifier")?)?;
    let sdp = decode_string(
        required(&map, 2, "SDP answer")?,
        MAX_SDP_BYTES,
        "SDP answer",
    )?;
    let candidates = decode_candidates(&map, 3)?;
    validate_sdp_and_candidates(&sdp, &candidates)?;
    Ok(Answer {
        session_id,
        sdp,
        candidates,
    })
}

fn decode_streaming_control(value: &[u8]) -> Result<(SessionId, u8), Error> {
    let map = parse_map(value)?;
    let session_id = decode_session_id(required(&map, 1, "session identifier")?)?;
    let command = required_u8(&map, 2, "streaming command")?;
    Ok((session_id, command))
}

fn decode_reoffer(value: &[u8]) -> Result<Reoffer, Error> {
    let map = parse_map(value)?;
    let session_id = decode_session_id(required(&map, 1, "session identifier")?)?;
    let sdp = decode_string(required(&map, 2, "SDP offer")?, MAX_SDP_BYTES, "SDP offer")?;
    validate_sdp_and_candidates(&sdp, &[])?;
    let options = required(&map, 3, "offer options")?;
    let options = decode_nested_offer_options(options)?;
    Ok(Reoffer {
        session_id,
        sdp,
        options,
    })
}

fn decode_candidates(map: &Tlv8Map, candidate_type: u8) -> Result<Vec<IceCandidate>, Error> {
    let mut candidates = Vec::new();
    for (_, value) in map
        .items()
        .iter()
        .filter(|(field_type, _)| *field_type == candidate_type)
    {
        if candidates.len() == MAX_CANDIDATES {
            return Err(Error::TooManyCandidates);
        }
        let candidate = Tlv8Map::parse(value).map_err(|_| Error::MalformedTlv)?;
        let candidate_value = decode_string(
            required(&candidate, 1, "ICE candidate")?,
            MAX_CANDIDATE_BYTES,
            "ICE candidate",
        )?;
        let sdp_mid = candidate
            .get_unique(2)
            .map_err(|_| Error::MalformedTlv)?
            .map(|value| decode_string(value, MAX_CANDIDATE_BYTES, "SDP mid"))
            .transpose()?;
        let sdp_mline_index = optional_u16(&candidate, 3, "SDP m-line index")?;
        candidates.push(IceCandidate {
            candidate: candidate_value,
            sdp_mid,
            sdp_mline_index,
        });
    }
    Ok(candidates)
}

fn encode_solicit_response(
    session_id: SessionId,
    status: SolicitStatus,
    offer: Option<&OfferDescription>,
) -> Result<Vec<u8>, Error> {
    let mut value = Vec::new();
    let mut writer = Tlv8Writer::new(&mut value);
    writer.push(1, session_id.as_bytes());
    if let Some(offer) = offer {
        validate_sdp_and_candidates(&offer.sdp, &offer.candidates)?;
        writer.push_str(2, &offer.sdp);
        for (index, candidate) in offer.candidates.iter().enumerate() {
            if index > 0 {
                writer.push_list_separator();
            }
            writer.push(3, &encode_candidate(candidate)?);
        }
    }
    writer.push_u8(4, status as u8);
    Ok(value)
}

fn encode_status_response(session_id: SessionId, status: StreamingStatus) -> Vec<u8> {
    let mut value = Vec::new();
    let mut writer = Tlv8Writer::new(&mut value);
    writer.push(1, session_id.as_bytes());
    writer.push_u8(2, status as u8);
    value
}

fn encode_reoffer_response(
    session_id: SessionId,
    status: StreamingStatus,
    answer: Option<&str>,
) -> Result<Vec<u8>, Error> {
    if let Some(answer) = answer {
        validate_sdp_and_candidates(answer, &[])?;
    }
    let mut value = Vec::new();
    let mut writer = Tlv8Writer::new(&mut value);
    writer.push(1, session_id.as_bytes());
    if let Some(answer) = answer {
        writer.push_str(2, answer);
    }
    writer.push_u8(3, status as u8);
    Ok(value)
}

fn encode_candidate(candidate: &IceCandidate) -> Result<Vec<u8>, Error> {
    validate_candidate(candidate)?;
    let mut value = Vec::new();
    let mut writer = Tlv8Writer::new(&mut value);
    writer.push_str(1, &candidate.candidate);
    if let Some(sdp_mid) = &candidate.sdp_mid {
        writer.push_str(2, sdp_mid);
    }
    if let Some(sdp_mline_index) = candidate.sdp_mline_index {
        writer.push_u16(3, sdp_mline_index);
    }
    Ok(value)
}

fn parse_map(value: &[u8]) -> Result<Tlv8Map, Error> {
    if value.len() > MAX_CHARACTERISTIC_BYTES {
        return Err(Error::ValueTooLarge {
            field: "characteristic",
            actual: value.len(),
            maximum: MAX_CHARACTERISTIC_BYTES,
        });
    }
    Tlv8Map::parse(value).map_err(|_| Error::MalformedTlv)
}

fn required<'a>(map: &'a Tlv8Map, field_type: u8, field: &'static str) -> Result<&'a [u8], Error> {
    map.get_unique(field_type)
        .map_err(|_| Error::MalformedTlv)?
        .ok_or(Error::MissingField(field))
}

fn required_u8(map: &Tlv8Map, field_type: u8, field: &'static str) -> Result<u8, Error> {
    match map.get_u8(field_type) {
        Ok(Some(value)) => Ok(value),
        Ok(None) => Err(Error::MissingField(field)),
        Err(Tlv8Error::InvalidIntegerWidth { expected, actual }) => {
            Err(Error::InvalidIntegerWidth {
                field,
                expected,
                actual,
            })
        }
        Err(_) => Err(Error::MalformedTlv),
    }
}

fn optional_u16(map: &Tlv8Map, field_type: u8, field: &'static str) -> Result<Option<u16>, Error> {
    let Some(value) = map
        .get_unique(field_type)
        .map_err(|_| Error::MalformedTlv)?
    else {
        return Ok(None);
    };
    let [low, high] = value else {
        return Err(Error::InvalidIntegerWidth {
            field,
            expected: 2,
            actual: value.len(),
        });
    };
    Ok(Some(u16::from_le_bytes([*low, *high])))
}

fn decode_session_id(value: &[u8]) -> Result<SessionId, Error> {
    let bytes = value
        .try_into()
        .map_err(|_| Error::InvalidSessionIdLength(value.len()))?;
    Ok(SessionId::new(bytes))
}

fn decode_string(value: &[u8], maximum: usize, field: &'static str) -> Result<String, Error> {
    if value.len() > maximum {
        return Err(Error::ValueTooLarge {
            field,
            actual: value.len(),
            maximum,
        });
    }
    std::str::from_utf8(value)
        .map(str::to_owned)
        .map_err(|_| Error::InvalidUtf8(field))
}

fn validate_sdp_and_candidates(sdp: &str, candidates: &[IceCandidate]) -> Result<(), Error> {
    if sdp.len() > MAX_SDP_BYTES {
        return Err(Error::ValueTooLarge {
            field: "SDP",
            actual: sdp.len(),
            maximum: MAX_SDP_BYTES,
        });
    }
    if !sdp.starts_with("v=0\r\n")
        || sdp.as_bytes().contains(&0)
        || sdp.as_bytes().iter().enumerate().any(|(index, byte)| {
            (*byte == b'\n'
                && index
                    .checked_sub(1)
                    .is_none_or(|i| sdp.as_bytes()[i] != b'\r'))
                || (*byte == b'\r' && sdp.as_bytes().get(index + 1) != Some(&b'\n'))
        })
    {
        return Err(Error::InvalidSdp);
    }
    if candidates.len() > MAX_CANDIDATES {
        return Err(Error::TooManyCandidates);
    }
    for candidate in candidates {
        validate_candidate(candidate)?;
    }
    Ok(())
}

fn validate_candidate(candidate: &IceCandidate) -> Result<(), Error> {
    if candidate.candidate.len() > MAX_CANDIDATE_BYTES {
        return Err(Error::ValueTooLarge {
            field: "ICE candidate",
            actual: candidate.candidate.len(),
            maximum: MAX_CANDIDATE_BYTES,
        });
    }
    if !candidate.candidate.starts_with("candidate:")
        || candidate
            .candidate
            .as_bytes()
            .iter()
            .any(|byte| matches!(byte, 0 | b'\r' | b'\n'))
    {
        return Err(Error::InvalidCandidate);
    }
    if candidate
        .sdp_mid
        .as_ref()
        .is_some_and(|sdp_mid| sdp_mid.len() > MAX_CANDIDATE_BYTES)
    {
        return Err(Error::ValueTooLarge {
            field: "SDP mid",
            actual: candidate.sdp_mid.as_ref().map_or(0, String::len),
            maximum: MAX_CANDIDATE_BYTES,
        });
    }
    if candidate.sdp_mid.as_ref().is_some_and(|sdp_mid| {
        sdp_mid
            .as_bytes()
            .iter()
            .any(|byte| matches!(byte, 0 | b'\r' | b'\n'))
    }) {
        return Err(Error::InvalidCandidate);
    }
    Ok(())
}

/// Invalid signaling input or state transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Previous outputs must be drained before another mutation.
    OutputNotDrained,
    /// TLV8 framing or integer width is invalid.
    MalformedTlv,
    /// Required protocol field is absent.
    MissingField(&'static str),
    /// A boolean field was neither zero nor one.
    InvalidBoolean(u8),
    /// A fixed-width integer had the wrong number of bytes.
    InvalidIntegerWidth {
        /// Field name.
        field: &'static str,
        /// Required width in bytes.
        expected: usize,
        /// Supplied width in bytes.
        actual: usize,
    },
    /// Session identifier was not exactly sixteen bytes.
    InvalidSessionIdLength(usize),
    /// A string field was not valid UTF-8.
    InvalidUtf8(&'static str),
    /// SDP did not have canonical RFC 8866 line framing.
    InvalidSdp,
    /// ICE candidate or media identifier was not a single valid signaling line.
    InvalidCandidate,
    /// A bounded field exceeded its protocol limit.
    ValueTooLarge {
        /// Field name.
        field: &'static str,
        /// Actual byte length.
        actual: usize,
        /// Accepted maximum.
        maximum: usize,
    },
    /// Candidate list exceeded its bound.
    TooManyCandidates,
    /// Caller reused a live session UUID.
    DuplicateSession(SessionId),
    /// Transport result referenced no current session.
    UnknownSession(SessionId),
    /// Transport result did not match the request awaiting it.
    MismatchedRequest {
        /// Request expected by the session.
        expected: RequestId,
        /// Request supplied by the adapter.
        actual: RequestId,
    },
    /// Session received a transport result in the wrong state.
    InvalidTransition {
        /// Affected session.
        session_id: SessionId,
        /// State required by the input.
        expected: SessionState,
        /// Current state.
        actual: SessionState,
    },
    /// Streaming control command is not implemented by the specification slice.
    UnsupportedCommand(u8),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutputNotDrained => f.write_str("pending outputs must be drained before input"),
            Self::MalformedTlv => f.write_str("malformed HAP TLV8 value"),
            Self::MissingField(field) => write!(f, "missing {field}"),
            Self::InvalidBoolean(value) => write!(f, "invalid boolean value {value}"),
            Self::InvalidIntegerWidth {
                field,
                expected,
                actual,
            } => write!(f, "{field} has {actual} bytes; expected exactly {expected}"),
            Self::InvalidSessionIdLength(length) => {
                write!(f, "session identifier has {length} bytes; expected 16")
            }
            Self::InvalidUtf8(field) => write!(f, "{field} is not valid UTF-8"),
            Self::InvalidSdp => f.write_str("invalid SDP framing"),
            Self::InvalidCandidate => f.write_str("invalid ICE candidate"),
            Self::ValueTooLarge {
                field,
                actual,
                maximum,
            } => write!(f, "{field} has {actual} bytes; maximum is {maximum}"),
            Self::TooManyCandidates => write!(f, "more than {MAX_CANDIDATES} ICE candidates"),
            Self::DuplicateSession(session_id) => {
                write!(f, "duplicate session {:02x?}", session_id.as_bytes())
            }
            Self::UnknownSession(session_id) => {
                write!(f, "unknown session {:02x?}", session_id.as_bytes())
            }
            Self::MismatchedRequest { expected, actual } => {
                write!(
                    f,
                    "request {} does not match pending request {}",
                    actual.0, expected.0
                )
            }
            Self::InvalidTransition {
                session_id,
                expected,
                actual,
            } => write!(
                f,
                "session {:02x?} is {actual:?}; expected {expected:?}",
                session_id.as_bytes()
            ),
            Self::UnsupportedCommand(command) => {
                write!(f, "unsupported WebRTC streaming command {command}")
            }
        }
    }
}

impl StdError for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_id(value: u8) -> SessionId {
        SessionId::new([value; 16])
    }

    fn solicit_value(sframe_enabled: bool) -> Vec<u8> {
        let options = offer_options_value(sframe_enabled);
        let mut value = Vec::new();
        Tlv8Writer::new(&mut value).push(1, &options);
        value
    }

    fn offer_options_value(sframe_enabled: bool) -> Vec<u8> {
        let mut options = Vec::new();
        Tlv8Writer::new(&mut options).push_u8(1, u8::from(sframe_enabled));
        options
    }

    fn answer_value(session_id: SessionId, sdp: &str, candidates: &[IceCandidate]) -> Vec<u8> {
        let mut value = Vec::new();
        let mut writer = Tlv8Writer::new(&mut value);
        writer.push(1, session_id.as_bytes());
        writer.push_str(2, sdp);
        for (index, candidate) in candidates.iter().enumerate() {
            if index > 0 {
                writer.push_list_separator();
            }
            writer.push(3, &encode_candidate(candidate).unwrap());
        }
        value
    }

    fn end_value(session_id: SessionId) -> Vec<u8> {
        let mut value = Vec::new();
        let mut writer = Tlv8Writer::new(&mut value);
        writer.push(1, session_id.as_bytes());
        writer.push_u8(2, 1);
        value
    }

    fn drain(device: &mut WebRtcDevice) -> Vec<Output> {
        let mut outputs = Vec::new();
        loop {
            match device.poll_output() {
                Output::Idle => return outputs,
                output => outputs.push(output),
            }
        }
    }

    fn create_offer(device: &mut WebRtcDevice, request_id: RequestId, session_id: SessionId) {
        let value = solicit_value(false);
        device
            .handle_input(Input::SolicitOffer {
                request_id,
                session_id,
                value: &value,
            })
            .unwrap();
        assert_eq!(
            drain(device),
            vec![Output::Action(Action::CreateOffer {
                request_id,
                session_id,
                options: OfferOptions {
                    sframe_enabled: false,
                },
            })]
        );
        device
            .handle_input(Input::OfferCreated {
                request_id,
                session_id,
                offer: Some(OfferDescription {
                    sdp: "v=0\r\n".to_owned(),
                    candidates: vec![
                        IceCandidate {
                            candidate: "candidate:1 1 UDP 1 192.0.2.1 5000 typ host".to_owned(),
                            sdp_mid: Some("video".to_owned()),
                            sdp_mline_index: Some(0),
                        },
                        IceCandidate {
                            candidate: "candidate:2 1 UDP 1 192.0.2.2 5001 typ host".to_owned(),
                            sdp_mid: Some("video".to_owned()),
                            sdp_mline_index: Some(0),
                        },
                    ],
                }),
            })
            .unwrap();
        let outputs = drain(device);
        let Output::WriteResponse(response) = &outputs[0] else {
            panic!("expected write response");
        };
        let map = Tlv8Map::parse(&response.value).unwrap();
        assert_eq!(
            map.get_unique(1).unwrap(),
            Some(session_id.as_bytes().as_slice())
        );
        assert_eq!(map.get_unique(2).unwrap(), Some(b"v=0\r\n".as_slice()));
        assert_eq!(map.get_u8(4).unwrap(), Some(0));
        assert_eq!(
            map.items()
                .iter()
                .filter(|(item_type, _)| *item_type == 3)
                .count(),
            2
        );
        assert_eq!(
            map.items()
                .iter()
                .filter(|(item_type, value)| *item_type == 0 && value.is_empty())
                .count(),
            1
        );
        assert_eq!(
            device.session_state(session_id),
            Some(SessionState::AwaitingAnswer)
        );
    }

    fn activate_session(device: &mut WebRtcDevice, session_id: SessionId) {
        create_offer(device, RequestId(1), session_id);
        let answer = answer_value(session_id, "v=0\r\na=answer\r\n", &[]);
        device
            .handle_input(Input::ProvideAnswer {
                request_id: RequestId(2),
                value: &answer,
            })
            .unwrap();
        drain(device);
        device
            .handle_input(Input::AnswerApplied {
                request_id: RequestId(2),
                session_id,
                success: true,
            })
            .unwrap();
        drain(device);
        device
            .handle_input(Input::TransportConnected { session_id })
            .unwrap();
        drain(device);
    }

    #[test]
    fn drives_offer_answer_and_connection_without_io() {
        let mut device = WebRtcDevice::new();
        let session_id = session_id(7);
        create_offer(&mut device, RequestId(1), session_id);

        let candidates = vec![IceCandidate {
            candidate: "candidate:2 1 UDP 1 192.0.2.2 5001 typ host".to_owned(),
            sdp_mid: Some("video".to_owned()),
            sdp_mline_index: Some(0),
        }];
        let answer = answer_value(session_id, "v=0\r\na=answer\r\n", &candidates);
        device
            .handle_input(Input::ProvideAnswer {
                request_id: RequestId(2),
                value: &answer,
            })
            .unwrap();
        assert_eq!(
            drain(&mut device),
            vec![Output::Action(Action::ApplyAnswer {
                request_id: RequestId(2),
                session_id,
                sdp: "v=0\r\na=answer\r\n".to_owned(),
                candidates,
            })]
        );

        device
            .handle_input(Input::AnswerApplied {
                request_id: RequestId(2),
                session_id,
                success: true,
            })
            .unwrap();
        let outputs = drain(&mut device);
        let Output::WriteResponse(response) = &outputs[0] else {
            panic!("expected answer response");
        };
        let map = Tlv8Map::parse(&response.value).unwrap();
        assert_eq!(map.get_u8(2).unwrap(), Some(0));

        device
            .handle_input(Input::TransportConnected { session_id })
            .unwrap();
        assert_eq!(
            drain(&mut device),
            vec![Output::Event(Event::ActiveSessionsChanged(1))]
        );
        assert_eq!(device.active_session_count(), 1);
    }

    #[test]
    fn accepts_six_concurrent_sessions_and_rejects_the_seventh() {
        let mut device = WebRtcDevice::new();

        for value in 1..=6 {
            create_offer(&mut device, RequestId(u64::from(value)), session_id(value));
        }
        assert_eq!(device.session_ids().len(), 6);

        let rejected_session = session_id(7);
        let value = solicit_value(false);
        device
            .handle_input(Input::SolicitOffer {
                request_id: RequestId(7),
                session_id: rejected_session,
                value: &value,
            })
            .unwrap();
        let outputs = drain(&mut device);
        let Output::WriteResponse(response) = &outputs[0] else {
            panic!("expected write response");
        };
        let response = Tlv8Map::parse(&response.value).unwrap();
        assert_eq!(
            response.get_u8(4).unwrap(),
            Some(SolicitStatus::Error as u8)
        );
        assert_eq!(device.session_state(rejected_session), None);
    }

    #[test]
    fn ends_session_and_updates_active_count() {
        let mut device = WebRtcDevice::new();
        let session_id = session_id(8);
        create_offer(&mut device, RequestId(1), session_id);
        let answer = answer_value(session_id, "v=0\r\n", &[]);
        device
            .handle_input(Input::ProvideAnswer {
                request_id: RequestId(2),
                value: &answer,
            })
            .unwrap();
        drain(&mut device);
        device
            .handle_input(Input::AnswerApplied {
                request_id: RequestId(2),
                session_id,
                success: true,
            })
            .unwrap();
        drain(&mut device);
        device
            .handle_input(Input::TransportConnected { session_id })
            .unwrap();
        drain(&mut device);

        let end = end_value(session_id);
        device
            .handle_input(Input::StreamingControl {
                request_id: RequestId(3),
                value: &end,
            })
            .unwrap();
        let outputs = drain(&mut device);
        assert_eq!(
            outputs[0],
            Output::Action(Action::EndSession { session_id })
        );
        let Output::WriteResponse(response) = &outputs[1] else {
            panic!("expected control response");
        };
        assert_eq!(response.characteristic, Characteristic::StreamingControl);
        assert_eq!(outputs[2], Output::Event(Event::ActiveSessionsChanged(0)));
    }

    #[test]
    fn rejects_sframe_without_creating_transport() {
        let mut device = WebRtcDevice::new();
        let value = solicit_value(true);
        device
            .handle_input(Input::SolicitOffer {
                request_id: RequestId(1),
                session_id: session_id(9),
                value: &value,
            })
            .unwrap();
        let outputs = drain(&mut device);
        let Output::WriteResponse(response) = &outputs[0] else {
            panic!("expected error response");
        };
        assert_eq!(
            Tlv8Map::parse(&response.value).unwrap().get_u8(4).unwrap(),
            Some(2)
        );
    }

    #[test]
    fn requires_outputs_to_be_drained() {
        let mut device = WebRtcDevice::new();
        let value = solicit_value(false);
        device
            .handle_input(Input::SolicitOffer {
                request_id: RequestId(1),
                session_id: session_id(1),
                value: &value,
            })
            .unwrap();
        assert_eq!(
            device.handle_input(Input::TransportClosed {
                session_id: session_id(1),
            }),
            Err(Error::OutputNotDrained)
        );
    }

    #[test]
    fn rejects_noncanonical_integer_widths() {
        let mut malformed_options = Vec::new();
        Tlv8Writer::new(&mut malformed_options).push(1, &[]);
        let mut solicit = Vec::new();
        Tlv8Writer::new(&mut solicit).push(1, &malformed_options);
        let mut device = WebRtcDevice::new();

        assert_eq!(
            device.handle_input(Input::SolicitOffer {
                request_id: RequestId(1),
                session_id: session_id(1),
                value: &solicit,
            }),
            Err(Error::InvalidIntegerWidth {
                field: "SFrame enabled",
                expected: 1,
                actual: 0,
            })
        );

        let mut candidate = Vec::new();
        let mut candidate_writer = Tlv8Writer::new(&mut candidate);
        candidate_writer.push_str(1, "candidate:1 1 UDP 1 192.0.2.1 5000 typ host");
        candidate_writer.push_u8(3, 0);
        let mut answer = Vec::new();
        let mut answer_writer = Tlv8Writer::new(&mut answer);
        answer_writer.push(1, session_id(1).as_bytes());
        answer_writer.push_str(2, "v=0\r\n");
        answer_writer.push(3, &candidate);

        assert_eq!(
            device.handle_input(Input::ProvideAnswer {
                request_id: RequestId(2),
                value: &answer,
            }),
            Err(Error::InvalidIntegerWidth {
                field: "SDP m-line index",
                expected: 2,
                actual: 1,
            })
        );
    }

    #[test]
    fn rejects_malformed_sdp_and_candidate_lines() {
        let mut device = WebRtcDevice::new();
        let session_id = session_id(2);
        let value = solicit_value(false);
        device
            .handle_input(Input::SolicitOffer {
                request_id: RequestId(1),
                session_id,
                value: &value,
            })
            .unwrap();
        drain(&mut device);

        assert_eq!(
            device.handle_input(Input::OfferCreated {
                request_id: RequestId(1),
                session_id,
                offer: Some(OfferDescription {
                    sdp: "v=0\n".to_owned(),
                    candidates: Vec::new(),
                }),
            }),
            Err(Error::InvalidSdp)
        );

        assert_eq!(
            validate_candidate(&IceCandidate {
                candidate: "candidate:1 1 UDP 1 host 5000 typ host\r\nattack".to_owned(),
                sdp_mid: None,
                sdp_mline_index: Some(0),
            }),
            Err(Error::InvalidCandidate)
        );
    }

    #[test]
    fn delegates_reoffer_and_returns_adapter_answer() {
        let mut device = WebRtcDevice::new();
        let session_id = session_id(3);
        activate_session(&mut device, session_id);
        let options = offer_options_value(false);
        let mut value = Vec::new();
        let mut writer = Tlv8Writer::new(&mut value);
        writer.push(1, session_id.as_bytes());
        writer.push_str(2, "v=0\r\na=reoffer\r\n");
        writer.push(3, &options);

        device
            .handle_input(Input::Reoffer {
                request_id: RequestId(3),
                value: &value,
            })
            .unwrap();
        assert_eq!(
            drain(&mut device),
            vec![Output::Action(Action::AcceptReoffer {
                request_id: RequestId(3),
                session_id,
                sdp: "v=0\r\na=reoffer\r\n".to_owned(),
            })]
        );
        assert_eq!(device.active_session_count(), 1);

        device
            .handle_input(Input::ReofferAnswered {
                request_id: RequestId(3),
                session_id,
                answer: Some("v=0\r\na=renegotiated\r\n".to_owned()),
            })
            .unwrap();
        let outputs = drain(&mut device);
        let Output::WriteResponse(response) = &outputs[0] else {
            panic!("expected reoffer response");
        };
        assert_eq!(response.characteristic, Characteristic::Reoffer);
        let response = Tlv8Map::parse(&response.value).unwrap();
        assert_eq!(response.get_u8(3).unwrap(), Some(0));
        assert_eq!(
            response.get_unique(2).unwrap(),
            Some(b"v=0\r\na=renegotiated\r\n".as_slice())
        );
        assert_eq!(device.session_state(session_id), Some(SessionState::Active));
    }

    #[test]
    fn update_session_reports_sframe_unsupported() {
        let mut device = WebRtcDevice::new();
        let session_id = session_id(4);
        activate_session(&mut device, session_id);
        let mut value = Vec::new();
        Tlv8Writer::new(&mut value).push(1, session_id.as_bytes());

        device
            .handle_input(Input::UpdateSession {
                request_id: RequestId(3),
                value: &value,
            })
            .unwrap();
        let outputs = drain(&mut device);
        let Output::WriteResponse(response) = &outputs[0] else {
            panic!("expected update response");
        };
        assert_eq!(response.characteristic, Characteristic::UpdateSession);
        assert_eq!(
            Tlv8Map::parse(&response.value).unwrap().get_u8(2).unwrap(),
            Some(StreamingStatus::Error as u8)
        );
    }
}
