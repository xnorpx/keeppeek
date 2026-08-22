//! Sans-I/O HomeKit camera protocol and str0m WebRTC session core.
//!
//! The caller owns HAP connections, sockets, scheduling, and threads. A
//! [`WebRtcDevice`] owns HomeKit signaling state and [`Str0mSession`] owns
//! WebRTC negotiation, ICE, DTLS-SRTP, RTP packetization, and media state.
//!
//! SFrame end-to-end media encryption is not currently supported. The WebRTC
//! state machine rejects SFrame offer requests and session key updates.

#![forbid(unsafe_code)]

mod crypto;
mod http;
mod legacy_rtp;
mod model;
mod pair_setup;
mod pair_verify;
mod pairings;
mod record;
mod rtp;
mod setup;
mod srp;
mod srtp;
mod str0m_session;
mod tlv8;
mod webrtc;

pub use http::{
    ContentType, Endpoint, HttpError, Method, ParseResult as HttpParseResult, Request, Response,
    Status, encode_event,
};
pub use legacy_rtp::{
    LegacyH264Level, LegacyH264Profile, LegacyRtpAddress, LegacyRtpError, LegacySessionId,
    LegacySrtpParameters, LegacyStreamCommand, LegacyVideoParameters, SelectedStreamConfiguration,
    SetupEndpointsRequest, SetupEndpointsResponse, decode_selected_stream, decode_setup_endpoints,
    encode_setup_endpoints_response,
};
pub use model::{
    AccessoryDatabase, AccessoryInformation, AudioTier, CameraConfig, ModelError, VideoCodec,
    VideoQuality, VideoTier,
};
pub use pair_setup::{
    PairSetup, PairSetupError, PairSetupInput, PairSetupOutput, PairSetupState, PairingStoreResult,
};
pub use pair_verify::{
    AccessoryIdentity, ControllerPairing, PairVerify, PairVerifyError, PairVerifyInput,
    PairVerifyOutput, PairVerifyState,
};
pub use pairings::{
    Pairings, PairingsError, PairingsInput, PairingsOutput, PairingsState, PairingsStoreResult,
};
pub use record::{DecodeResult, RecordDecoder, RecordEncoder, RecordError, SessionKeys};
pub use rtp::{H264Packetizer, PacketizeError};
pub use setup::{AccessoryCategory, AccessoryId, BonjourStatus, SetupCode, SetupId, SetupPayload};
pub use srtp::{AUTH_TAG_LEN as SRTP_AUTH_TAG_LEN, SrtcpSession, SrtpError, SrtpSession};
pub use str0m_session::{Str0mSession, Str0mSessionError};
pub use webrtc::{
    Action, Characteristic, Error, Event, IceCandidate, Input, OfferDescription, OfferOptions,
    Output, RequestId, SessionId, SessionState, StreamingStatus, WebRtcDevice, WriteResponse,
};
