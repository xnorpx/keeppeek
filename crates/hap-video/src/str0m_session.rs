use crate::{IceCandidate, OfferDescription, VideoCodec};
use std::{error::Error as StdError, fmt, sync::Arc, time::Instant};
use str0m::{
    Candidate, Event, Input, Output, Rtc,
    bwe::Bitrate,
    change::{SdpAnswer, SdpOffer, SdpPendingOffer},
    format::Codec,
    media::{Direction, MediaKind, MediaTime, Mid},
};

/// A negotiation-aware WebRTC session backed by str0m.
///
/// The caller owns sockets, timers, and scheduling. This type owns the complete
/// WebRTC protocol state, including the pending local offer needed to accept a
/// later answer.
pub struct Str0mSession {
    rtc: Rtc,
    pending_offer: Option<SdpPendingOffer>,
    video_mid: Option<Mid>,
    connected: bool,
    closed: bool,
}

impl Str0mSession {
    /// Creates a camera-offerer session with one send-only video track.
    pub fn create_video_offer(
        mut rtc: Rtc,
        local_candidates: Vec<Candidate>,
        stream_id: Option<String>,
        track_id: Option<String>,
    ) -> Result<(Self, OfferDescription), Str0mSessionError> {
        let additional_candidates = local_candidates
            .iter()
            .map(|candidate| IceCandidate {
                candidate: candidate.to_sdp_string(),
                sdp_mid: None,
                sdp_mline_index: Some(0),
            })
            .collect();
        for candidate in local_candidates {
            rtc.add_local_candidate(candidate);
        }
        let mut changes = rtc.sdp_api();
        changes.add_media(
            MediaKind::Video,
            Direction::SendOnly,
            stream_id,
            track_id,
            None,
        );
        let (offer, pending_offer) = changes.apply().ok_or(Str0mSessionError::OfferNotCreated)?;
        let description = OfferDescription {
            sdp: offer.to_sdp_string(),
            candidates: additional_candidates,
        };
        Ok((
            Self {
                rtc,
                pending_offer: Some(pending_offer),
                video_mid: None,
                connected: false,
                closed: false,
            },
            description,
        ))
    }

    /// Creates an answerer session from a remote SDP offer.
    pub fn accept_video_offer(
        mut rtc: Rtc,
        local_candidates: Vec<Candidate>,
        offer: &str,
    ) -> Result<(Self, String), Str0mSessionError> {
        for candidate in local_candidates {
            rtc.add_local_candidate(candidate);
        }
        let offer = SdpOffer::from_sdp_string(offer)
            .map_err(|error| Str0mSessionError::Sdp(error.to_string()))?;
        let answer = rtc
            .sdp_api()
            .accept_offer(offer)
            .map_err(|error| Str0mSessionError::Rtc(error.to_string()))?;
        Ok((
            Self {
                rtc,
                pending_offer: None,
                video_mid: None,
                connected: false,
                closed: false,
            },
            answer.to_sdp_string(),
        ))
    }

    /// Applies the answer corresponding to the locally generated offer.
    pub fn apply_answer(
        &mut self,
        answer: &str,
        additional_candidates: &[IceCandidate],
    ) -> Result<(), Str0mSessionError> {
        let answer = SdpAnswer::from_sdp_string(answer)
            .map_err(|error| Str0mSessionError::Sdp(error.to_string()))?;
        let pending = self
            .pending_offer
            .take()
            .ok_or(Str0mSessionError::NoPendingOffer)?;
        self.rtc
            .sdp_api()
            .accept_answer(pending, answer)
            .map_err(|error| Str0mSessionError::Rtc(error.to_string()))?;
        self.add_remote_candidates(additional_candidates)
    }

    /// Accepts a controller-generated renegotiation offer.
    pub fn accept_reoffer(&mut self, offer: &str) -> Result<String, Str0mSessionError> {
        if self.pending_offer.is_some() {
            return Err(Str0mSessionError::OfferPending);
        }
        let offer = SdpOffer::from_sdp_string(offer)
            .map_err(|error| Str0mSessionError::Sdp(error.to_string()))?;
        self.rtc
            .sdp_api()
            .accept_offer(offer)
            .map(|answer| answer.to_sdp_string())
            .map_err(|error| Str0mSessionError::Rtc(error.to_string()))
    }

    /// Applies one network packet or timeout to str0m.
    pub fn handle_input(&mut self, input: Input<'_>) -> Result<(), Str0mSessionError> {
        self.rtc
            .handle_input(input)
            .map_err(|error| Str0mSessionError::Rtc(error.to_string()))
    }

    /// Polls one str0m output and updates observable transport state.
    pub fn poll_output(&mut self) -> Result<Output, Str0mSessionError> {
        let output = self
            .rtc
            .poll_output()
            .map_err(|error| Str0mSessionError::Rtc(error.to_string()))?;
        match &output {
            Output::Event(Event::Connected) => self.connected = true,
            Output::Event(Event::Closed) => self.closed = true,
            Output::Event(Event::MediaAdded(media)) if media.kind == MediaKind::Video => {
                self.video_mid = Some(media.mid);
            }
            _ => {}
        }
        Ok(output)
    }

    /// Writes one encoded Annex B video access unit to the negotiated track.
    pub fn write_video(
        &mut self,
        codec: VideoCodec,
        h264_profile_level_id: Option<u32>,
        wallclock: Instant,
        media_time: MediaTime,
        data: Arc<[u8]>,
    ) -> Result<bool, Str0mSessionError> {
        let Some(mid) = self.video_mid else {
            return Ok(false);
        };
        let codec = match codec {
            VideoCodec::H264 => Codec::H264,
            VideoCodec::H265 => Codec::H265,
        };
        let payload_type =
            self.rtc.writer(mid).and_then(|writer| {
                writer
                    .payload_params()
                    .find(|params| {
                        params.spec().codec == codec
                            && (codec != Codec::H264
                                || (params.spec().format.packetization_mode == Some(1)
                                    && h264_profile_level_id.is_none_or(|source| {
                                        params.spec().format.profile_level_id.is_some_and(
                                            |payload| h264_profiles_match(source, payload),
                                        )
                                    })))
                    })
                    .map(|params| params.pt())
            });
        let Some(payload_type) = payload_type else {
            return Ok(false);
        };
        self.rtc
            .writer(mid)
            .ok_or(Str0mSessionError::VideoMediaMissing)?
            .write(payload_type, wallclock, media_time, data)
            .map_err(|error| Str0mSessionError::Rtc(error.to_string()))?;
        Ok(true)
    }

    /// Sets str0m's desired aggregate egress bitrate.
    pub fn set_desired_bitrate(&mut self, bitrate: Bitrate) {
        self.rtc.bwe().set_desired_bitrate(bitrate);
    }

    pub const fn is_connected(&self) -> bool {
        self.connected
    }

    pub const fn is_closed(&self) -> bool {
        self.closed
    }

    fn add_remote_candidates(
        &mut self,
        candidates: &[IceCandidate],
    ) -> Result<(), Str0mSessionError> {
        for candidate in candidates {
            let candidate = Candidate::from_sdp_string(&candidate.candidate)
                .map_err(|error| Str0mSessionError::Candidate(error.to_string()))?;
            self.rtc.add_remote_candidate(candidate);
        }
        Ok(())
    }
}

impl fmt::Debug for Str0mSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Str0mSession")
            .field("has_pending_offer", &self.pending_offer.is_some())
            .field("video_mid", &self.video_mid)
            .field("connected", &self.connected)
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Str0mSessionError {
    Sdp(String),
    Candidate(String),
    Rtc(String),
    OfferNotCreated,
    NoPendingOffer,
    OfferPending,
    VideoMediaMissing,
}

impl fmt::Display for Str0mSessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sdp(error) => write!(f, "invalid WebRTC SDP: {error}"),
            Self::Candidate(error) => write!(f, "invalid ICE candidate: {error}"),
            Self::Rtc(error) => write!(f, "str0m session error: {error}"),
            Self::OfferNotCreated => f.write_str("str0m did not create an SDP offer"),
            Self::NoPendingOffer => f.write_str("no local SDP offer is awaiting an answer"),
            Self::OfferPending => f.write_str("a local SDP offer is still pending"),
            Self::VideoMediaMissing => f.write_str("negotiated video media disappeared"),
        }
    }
}

impl StdError for Str0mSessionError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum H264Profile {
    ConstrainedBaseline,
    Baseline,
    Main,
    Extended,
    High,
    ConstrainedHigh,
    High10,
    High422,
    High444Predictive,
    High10Intra,
    High422Intra,
    High444Intra,
    Cavlc444Intra,
}

const fn h264_profile(profile_level_id: u32) -> Option<H264Profile> {
    let [_, profile_idc, profile_iop, _] = profile_level_id.to_be_bytes();
    match (profile_idc, profile_iop) {
        (0x42, profile_iop) if profile_iop & 0x4f == 0x40 => Some(H264Profile::ConstrainedBaseline),
        (0x4d, profile_iop) if profile_iop & 0x8f == 0x80 => Some(H264Profile::ConstrainedBaseline),
        (0x58, profile_iop) if profile_iop & 0xcf == 0xc0 => Some(H264Profile::ConstrainedBaseline),
        (0x42, profile_iop) if profile_iop & 0x4f == 0 => Some(H264Profile::Baseline),
        (0x58, profile_iop) if profile_iop & 0xcf == 0x80 => Some(H264Profile::Baseline),
        (0x4d, profile_iop) if profile_iop & 0xaf == 0 => Some(H264Profile::Main),
        (0x58, profile_iop) if profile_iop & 0xcf == 0 => Some(H264Profile::Extended),
        (0x64, 0) => Some(H264Profile::High),
        (0x64, 0x0c) => Some(H264Profile::ConstrainedHigh),
        (0x6e, 0) => Some(H264Profile::High10),
        (0x7a, 0) => Some(H264Profile::High422),
        (0xf4, 0) => Some(H264Profile::High444Predictive),
        (0x6e, 0x10) => Some(H264Profile::High10Intra),
        (0x7a, 0x10) => Some(H264Profile::High422Intra),
        (0xf4, 0x10) => Some(H264Profile::High444Intra),
        (0x2c, 0x10) => Some(H264Profile::Cavlc444Intra),
        _ => None,
    }
}

fn h264_profiles_match(source: u32, payload: u32) -> bool {
    h264_profile(source).is_some_and(|source_profile| h264_profile(payload) == Some(source_profile))
}
